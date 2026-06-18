//! SQLite-backed storage for encrypted credentials.
//!
//! Schema:
//! - `meta`: single-row table holding the Argon2 salt and schema version.
//! - `credentials`: one row per stored secret. The secret value is stored as
//!   an opaque encrypted blob (`nonce || ciphertext`); all other fields are
//!   plaintext so they can be filtered/searched without unlocking.

use crate::credential::CredentialKind;
use crate::crypto::{self, MasterKey, SALT_LEN};
use anyhow::{Context, Result};
use rusqlite::{Connection, params, params_from_iter};
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Current schema version, bumped on incompatible changes.
const SCHEMA_VERSION: u32 = 1;

/// Known plaintext sealed with the master key and stored in `meta` as a
/// password-verification probe. On open we try to decrypt it; failure means
/// the master password is wrong.
const PROBE_PLAINTEXT: &[u8] = b"devcred-probe-v1";

/// A credential row as stored in the database (secret still encrypted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub environment: String,
    pub project: String,
    /// Encrypted `nonce || ciphertext` blob.
    pub secret_blob: Vec<u8>,
    /// Suggested env var name, e.g. `GITHUB_TOKEN`.
    pub env_var: String,
    pub notes: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A decrypted credential, ready to copy or inject.
#[derive(Debug, Clone)]
pub struct DecryptedCredential {
    pub id: i64,
    pub name: String,
    pub kind: CredentialKind,
    pub environment: String,
    pub project: String,
    pub secret: String,
    pub env_var: String,
    pub notes: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// User-defined key/value pairs shown only in the detail view.
    pub custom_fields: Vec<CustomField>,
}

/// A user-defined field attached to a credential.
#[derive(Debug, Clone)]
pub struct CustomField {
    pub key: String,
    /// Decrypted value.
    pub value: String,
    /// Whether the value should be masked in the UI by default.
    pub masked: bool,
}

/// A handle to an open vault: the SQLite connection plus the derived key.
pub struct Vault {
    conn: Connection,
    key: MasterKey,
}

impl Vault {
    /// Open (or create) the vault at `path`, deriving the key from `password`.
    /// If the database is new, a fresh salt is generated and stored.
    pub fn open(path: &Path, password: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening vault at {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS meta (
                 key   TEXT PRIMARY KEY,
                 value BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS credentials (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 name        TEXT NOT NULL,
                 kind        TEXT NOT NULL,
                 environment TEXT NOT NULL DEFAULT '',
                 project     TEXT NOT NULL DEFAULT '',
                 secret_blob BLOB NOT NULL,
                 env_var     TEXT NOT NULL DEFAULT '',
                 notes       TEXT NOT NULL DEFAULT '',
                 created_at  INTEGER NOT NULL,
                 updated_at  INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_cred_env ON credentials(environment);
             CREATE INDEX IF NOT EXISTS idx_cred_project ON credentials(project);
             CREATE INDEX IF NOT EXISTS idx_cred_name ON credentials(name);
             CREATE TABLE IF NOT EXISTS custom_fields (
                 id            INTEGER PRIMARY KEY AUTOINCREMENT,
                 credential_id INTEGER NOT NULL REFERENCES credentials(id) ON DELETE CASCADE,
                 field_key     TEXT NOT NULL,
                 value_blob    BLOB NOT NULL,
                 masked        INTEGER NOT NULL DEFAULT 0,
                 position      INTEGER NOT NULL DEFAULT 0
             );",
        )
        .context("initializing schema")?;

        let salt: Vec<u8> = match conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'salt'",
                [],
                |r| r.get::<_, Vec<u8>>(0),
            ) {
            Ok(s) => s,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                let s = crypto::new_salt();
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('salt', ?1)",
                    params![s.as_slice()],
                )?;
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('version', ?1)",
                    params![SCHEMA_VERSION.to_le_bytes().as_slice()],
                )?;
                s.to_vec()
            }
            Err(e) => return Err(e).context("reading salt"),
        };
        if salt.len() < SALT_LEN {
            return Err(anyhow::anyhow!("stored salt is corrupt"));
        }

        let key = MasterKey::derive(password, &salt).context("deriving master key")?;

        // Verify the master password using an encrypted probe stored in meta.
        // New vaults get a probe created; existing vaults must decrypt it.
        // For vaults predating the probe (no `probe` row), fall back to
        // decrypting an existing credential's secret, then store a probe.
        let probe: Option<Vec<u8>> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'probe'",
                [],
                |r: &rusqlite::Row<'_>| r.get::<_, Vec<u8>>(0),
            )
            .ok();

        match probe {
            Some(blob) => {
                key.open(&blob)
                    .context("wrong master password (or corrupt vault)")?;
            }
            None => {
                let has_creds: bool = conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM credentials)",
                        [],
                        |r: &rusqlite::Row<'_>| r.get(0),
                    )
                    .unwrap_or(false);
                if has_creds {
                    let blob: Vec<u8> = conn.query_row(
                        "SELECT secret_blob FROM credentials ORDER BY id LIMIT 1",
                        [],
                        |r: &rusqlite::Row<'_>| r.get(0),
                    )?;
                    key.open(&blob)
                        .context("wrong master password (or corrupt vault)")?;
                }
                // Store a fresh probe for future opens.
                let probe_blob = key.seal(PROBE_PLAINTEXT)?;
                conn.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('probe', ?1)",
                    params![probe_blob.as_slice()],
                )?;
            }
        }

        Ok(Vault { conn, key })
    }

    /// Insert a new credential. Returns its row id.
    pub fn add(
        &self,
        name: &str,
        kind: CredentialKind,
        environment: &str,
        project: &str,
        secret: &str,
        env_var: &str,
        notes: &str,
    ) -> Result<i64> {
        let blob = self.key.seal(secret.as_bytes())?;
        let now = now_ts();
        self.conn.execute(
            "INSERT INTO credentials
                (name, kind, environment, project, secret_blob, env_var, notes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                name,
                kind_label(&kind),
                environment,
                project,
                blob,
                env_var,
                notes,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update an existing credential by id. Any `None` field is left unchanged.
    pub fn update(
        &self,
        id: i64,
        name: Option<&str>,
        environment: Option<&str>,
        project: Option<&str>,
        secret: Option<&str>,
        env_var: Option<&str>,
        notes: Option<&str>,
        kind: Option<CredentialKind>,
    ) -> Result<()> {
        let now = now_ts();
        let mut sets: Vec<&str> = Vec::new();
        let mut binds: Vec<Value> = Vec::new();
        if let Some(v) = name {
            sets.push("name = ?");
            binds.push(Value::Text(v.to_string()));
        }
        if let Some(k) = kind {
            sets.push("kind = ?");
            binds.push(Value::Text(kind_label(&k)));
        }
        if let Some(v) = environment {
            sets.push("environment = ?");
            binds.push(Value::Text(v.to_string()));
        }
        if let Some(v) = project {
            sets.push("project = ?");
            binds.push(Value::Text(v.to_string()));
        }
        if let Some(v) = env_var {
            sets.push("env_var = ?");
            binds.push(Value::Text(v.to_string()));
        }
        if let Some(v) = notes {
            sets.push("notes = ?");
            binds.push(Value::Text(v.to_string()));
        }
        // Seal the secret into a blob that outlives the `if let` block.
        let secret_blob: Option<Vec<u8>> = if let Some(v) = secret {
            Some(self.key.seal(v.as_bytes())?)
        } else {
            None
        };
        if let Some(ref blob) = secret_blob {
            sets.push("secret_blob = ?");
            binds.push(Value::Blob(blob.clone()));
        }
        sets.push("updated_at = ?");
        binds.push(Value::Integer(now));
        binds.push(Value::Integer(id));

        let sql = format!("UPDATE credentials SET {} WHERE id = ?", sets.join(", "));
        self.conn.execute(&sql, params_from_iter(binds))?;
        Ok(())
    }

    /// Delete a credential by id.
    pub fn delete(&self, id: i64) -> Result<bool> {
        let n = self.conn.execute("DELETE FROM credentials WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// List all credentials, optionally filtered by environment and project.
    /// Secrets stay encrypted.
    pub fn list(
        &self,
        environment: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<CredentialRecord>> {
        let mut sql = String::from("SELECT id, name, kind, environment, project, secret_blob, env_var, notes, created_at, updated_at FROM credentials");
        let mut clauses: Vec<String> = Vec::new();
        let mut binds: Vec<Value> = Vec::new();
        if let Some(e) = environment {
            if e == "*" || e.is_empty() {
                // no filter
            } else {
                clauses.push("environment = ?".to_string());
                binds.push(Value::Text(e.to_string()));
            }
        }
        if let Some(p) = project {
            if p == "*" || p.is_empty() {
            } else {
                clauses.push("project = ?".to_string());
                binds.push(Value::Text(p.to_string()));
            }
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY name COLLATE NOCASE ASC");

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(binds), row_to_record)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fetch a single credential by id (encrypted).
    pub fn get(&self, id: i64) -> Result<Option<CredentialRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, environment, project, secret_blob, env_var, notes, created_at, updated_at FROM credentials WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_record)?;
        if let Some(r) = rows.next() {
            Ok(Some(r?))
        } else {
            Ok(None)
        }
    }

    /// Fetch a single credential by name (encrypted).
    pub fn find_by_name(&self, name: &str) -> Result<Option<CredentialRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, environment, project, secret_blob, env_var, notes, created_at, updated_at FROM credentials WHERE name = ?1 COLLATE NOCASE LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![name], row_to_record)?;
        if let Some(r) = rows.next() {
            Ok(Some(r?))
        } else {
            Ok(None)
        }
    }

    /// Decrypt a record into a [`DecryptedCredential`].
    pub fn decrypt(&self, rec: &CredentialRecord) -> Result<DecryptedCredential> {
        let pt = self.key.open(&rec.secret_blob)?;
        let secret = String::from_utf8(pt).context("secret is not valid UTF-8")?;
        let custom_fields = self.load_custom_fields(rec.id).unwrap_or_default();
        Ok(DecryptedCredential {
            id: rec.id,
            name: rec.name.clone(),
            kind: parse_kind(&rec.kind),
            environment: rec.environment.clone(),
            project: rec.project.clone(),
            secret,
            env_var: rec.env_var.clone(),
            notes: rec.notes.clone(),
            created_at: rec.created_at,
            updated_at: rec.updated_at,
            custom_fields,
        })
    }

    /// Distinct environments present in the vault.
    pub fn environments(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT environment FROM credentials WHERE environment != '' ORDER BY environment COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Distinct projects present in the vault.
    pub fn projects(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT project FROM credentials WHERE project != '' ORDER BY project COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Count of stored credentials.
    pub fn count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM credentials", [], |r| r.get(0))?)
    }

    /// Verify a master password by re-deriving a key and attempting to decrypt
    /// a stored secret. Used to gate sensitive operations (e.g. revealing a
    /// secret in plaintext) behind a fresh confirmation.
    ///
    /// Returns `true` if the password is correct or the vault is empty.
    pub fn verify_password(&self, password: &str) -> bool {
        let salt: Vec<u8> = match self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'salt'", [], |r| {
                r.get::<_, Vec<u8>>(0)
            }) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let key = match MasterKey::derive(password, &salt) {
            Ok(k) => k,
            Err(_) => return false,
        };
        // Verify against the stored probe (works even for empty vaults).
        match self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'probe'",
            [],
            |r| r.get::<_, Vec<u8>>(0),
        ) {
            Ok(blob) => key.open(&blob).is_ok(),
            Err(_) => true,
        }
    }

    /// Replace all custom fields for a credential (delete + reinsert).
    /// Each tuple is `(key, value, masked)`.
    pub fn set_custom_fields(&self, credential_id: i64, fields: &[(String, String, bool)]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM custom_fields WHERE credential_id = ?1",
            params![credential_id],
        )?;
        for (i, (key, value, masked)) in fields.iter().enumerate() {
            if key.trim().is_empty() && value.trim().is_empty() {
                continue;
            }
            let blob = self.key.seal(value.as_bytes())?;
            self.conn.execute(
                "INSERT INTO custom_fields (credential_id, field_key, value_blob, masked, position)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![credential_id, key, blob, *masked as i64, i as i64],
            )?;
        }
        Ok(())
    }

    /// Load and decrypt all custom fields for a credential, ordered by position.
    fn load_custom_fields(&self, credential_id: i64) -> Result<Vec<CustomField>> {
        let mut stmt = self.conn.prepare(
            "SELECT field_key, value_blob, masked FROM custom_fields
             WHERE credential_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![credential_id], |r| {
            let key: String = r.get(0)?;
            let blob: Vec<u8> = r.get(1)?;
            let masked: i64 = r.get(2)?;
            Ok((key, blob, masked != 0))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (key, blob, masked) = row?;
            let pt = self.key.open(&blob)?;
            let value = String::from_utf8(pt).unwrap_or_default();
            out.push(CustomField { key, value, masked });
        }
        Ok(out)
    }
}

impl CredentialRecord {
    /// Parse the stored kind string into a [`CredentialKind`] enum.
    pub fn kind_enum(&self) -> CredentialKind {
        parse_kind(&self.kind)
    }
}

fn row_to_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<CredentialRecord> {
    Ok(CredentialRecord {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        environment: r.get(3)?,
        project: r.get(4)?,
        secret_blob: r.get(5)?,
        env_var: r.get(6)?,
        notes: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn kind_label(k: &CredentialKind) -> String {
    match k {
        CredentialKind::Github => "github".to_string(),
        CredentialKind::Pypi => "pypi".to_string(),
        CredentialKind::Npm => "npm".to_string(),
        CredentialKind::Gitlab => "gitlab".to_string(),
        CredentialKind::Docker => "docker".to_string(),
        CredentialKind::Aws => "aws".to_string(),
        CredentialKind::Stripe => "stripe".to_string(),
        CredentialKind::Slack => "slack".to_string(),
        CredentialKind::DigitalOcean => "digitalocean".to_string(),
        CredentialKind::Linear => "linear".to_string(),
        CredentialKind::Vercel => "vercel".to_string(),
        CredentialKind::TwoFactorRecovery => "2fa_recovery".to_string(),
        CredentialKind::Bearer => "bearer".to_string(),
        CredentialKind::Generic => "generic".to_string(),
        // Preserve the user-defined name verbatim.
        CredentialKind::Custom(s) => s.clone(),
    }
}

pub fn parse_kind(s: &str) -> CredentialKind {
    match s {
        "github" => CredentialKind::Github,
        "pypi" => CredentialKind::Pypi,
        "npm" => CredentialKind::Npm,
        "gitlab" => CredentialKind::Gitlab,
        "docker" => CredentialKind::Docker,
        "aws" => CredentialKind::Aws,
        "stripe" => CredentialKind::Stripe,
        "slack" => CredentialKind::Slack,
        "digitalocean" => CredentialKind::DigitalOcean,
        "linear" => CredentialKind::Linear,
        "vercel" => CredentialKind::Vercel,
        "2fa_recovery" => CredentialKind::TwoFactorRecovery,
        "bearer" => CredentialKind::Bearer,
        "generic" => CredentialKind::Generic,
        // Unknown strings become Custom so user-defined kinds round-trip.
        other => CredentialKind::Custom(other.to_string()),
    }
}

/// Resolve the default vault path under the user's config directory.
pub fn default_vault_path() -> Result<std::path::PathBuf> {
    let dir = dirs::config_dir().context("no config directory on this platform")?;
    let dir = dir.join("devcred");
    std::fs::create_dir_all(&dir).context("creating config directory")?;
    Ok(dir.join("vault.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_vault() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "devcred-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn full_crud_cycle() {
        let path = tmp_vault();
        let vault = Vault::open(&path, "master-pass").unwrap();

        // Add
        let id = vault
            .add(
                "github-personal",
                CredentialKind::Github,
                "prod",
                "web-app",
                "ghp_0123456789012345678901234567890123456789",
                "GITHUB_TOKEN",
                "personal access token",
            )
            .unwrap();
        assert_eq!(vault.count().unwrap(), 1);

        // List
        let all = vault.list(None, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "github-personal");
        assert_eq!(all[0].kind, "github");

        // Get by id
        let rec = vault.get(id).unwrap().unwrap();
        assert_eq!(rec.environment, "prod");

        // Get by name
        let rec2 = vault.find_by_name("GitHub-Personal").unwrap().unwrap();
        assert_eq!(rec2.id, id);

        // Decrypt
        let dec = vault.decrypt(&rec).unwrap();
        assert_eq!(dec.secret, "ghp_0123456789012345678901234567890123456789");
        assert_eq!(dec.kind, CredentialKind::Github);

        // Update
        vault
            .update(
                id,
                Some("github-work"),
                Some("staging"),
                None,
                Some("ghp_newsecret0123456789012345678901234567890"),
                None,
                Some("updated note"),
                None,
            )
            .unwrap();
        let rec3 = vault.get(id).unwrap().unwrap();
        assert_eq!(rec3.name, "github-work");
        assert_eq!(rec3.environment, "staging");
        let dec3 = vault.decrypt(&rec3).unwrap();
        assert_eq!(dec3.secret, "ghp_newsecret0123456789012345678901234567890");

        // Filter
        let filtered = vault.list(Some("prod"), None).unwrap();
        assert!(filtered.is_empty());
        let filtered = vault.list(Some("staging"), None).unwrap();
        assert_eq!(filtered.len(), 1);

        // Environments / projects
        assert_eq!(vault.environments().unwrap(), vec!["staging"]);
        assert_eq!(vault.projects().unwrap(), vec!["web-app"]);

        // Delete
        assert!(vault.delete(id).unwrap());
        assert_eq!(vault.count().unwrap(), 0);
        assert!(!vault.delete(id).unwrap());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_password_fails_to_decrypt() {
        let path = tmp_vault();
        let vault = Vault::open(&path, "correct").unwrap();
        let id = vault
            .add(
                "secret",
                CredentialKind::Generic,
                "",
                "",
                "my-secret-value",
                "API_KEY",
                "",
            )
            .unwrap();

        // Reopen with the wrong password — now rejected at open time by the
        // encrypted probe stored in meta.
        assert!(Vault::open(&path, "wrong").is_err());

        // Correct password works.
        let rec = vault.get(id).unwrap().unwrap();
        assert_eq!(vault.decrypt(&rec).unwrap().secret, "my-secret-value");

        std::fs::remove_file(&path).ok();
    }
}
