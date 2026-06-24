//! Command-line interface: clap subcommands for non-interactive use.
//!
//! `devcred` with no subcommand launches the TUI. The subcommands cover the
//! scriptable surface: `init`, `add`, `list`, `copy`, `inject`, `show`, `rm`.

use crate::credential;
use crate::db::{self, Vault};
use crate::{clipboard, injector};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::path::PathBuf;

/// DevCred — local, encrypted credential manager for developers.
#[derive(Parser, Debug)]
#[command(
    name = "devcred",
    version,
    about = "Local, encrypted credential manager with a TUI",
    long_about = "DevCred stores API keys, tokens, and 2FA recovery codes in an \
                  encrypted local SQLite vault. Run `devcred` with no arguments \
                  to launch the TUI, or use a subcommand for scripting."
)]
pub struct Cli {
    /// Path to the vault file. Defaults to ~/.config/devcred/vault.db
    #[arg(long, env = "DEVCRED_VAULT", global = true)]
    pub vault: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize a new vault (set the master password).
    Init,
    /// Add a credential. Without flags, prompts interactively.
    Add {
        /// Display name, e.g. "github-personal".
        #[arg(long)]
        name: Option<String>,
        /// Raw secret. If omitted, prompted securely.
        #[arg(long)]
        secret: Option<String>,
        /// Override the detected kind, e.g. "github" or a custom name like "kaggle".
        #[arg(long)]
        kind: Option<String>,
        /// Environment tag, e.g. "prod", "staging".
        #[arg(long)]
        env: Option<String>,
        /// Project group, e.g. "web-app".
        #[arg(long)]
        project: Option<String>,
        /// Override the detected env var name.
        #[arg(long = "env-var")]
        env_var: Option<String>,
        /// Free-form notes.
        #[arg(long)]
        notes: Option<String>,
        /// Custom field, repeatable. Format: "key=value" or "key=value:masked".
        /// The ":masked" suffix marks the value for masked display in the TUI.
        #[arg(long = "field", value_name = "KEY=VALUE[:masked]")]
        fields: Vec<String>,
    },
    /// List credentials (names only; secrets stay hidden).
    ///
    /// Use --query for fuzzy search across name, project, env, and custom fields.
    /// Prefix syntax: "env:staging", "pro:web-app", "cf:api-key".
    /// Without a prefix, the query searches all fields.
    List {
        /// Filter by environment.
        #[arg(long)]
        env: Option<String>,
        /// Filter by project.
        #[arg(long)]
        project: Option<String>,
        /// Fuzzy search query. Supports prefixes: env:, pro:, cf:.
        /// Without prefix, searches across all fields.
        #[arg(short, long)]
        query: Option<String>,
        /// Show the decrypted secret alongside each row (dangerous).
        #[arg(long)]
        reveal: bool,
    },
    /// Copy a credential's secret to the clipboard, auto-cleared after N seconds.
    Copy {
        /// Credential name or id.
        query: String,
        /// Seconds before the clipboard is cleared.
        #[arg(long, default_value_t = clipboard::DEFAULT_CLEAR_SECS)]
        clear_after: u64,
    },
    /// Print a credential's secret to stdout (pipe-friendly; use with care).
    ///
    /// The query supports fuzzy search across name, project, env, and custom fields.
    /// Prefix syntax: "env:staging", "pro:web-app", "cf:api-key".
    /// Without a prefix, the query searches all fields.
    /// If multiple credentials match, the best match is shown.
    ///
    /// Use --full to print all fields (name, kind, env, project, env_var,
    /// secret, notes, custom fields, timestamps) in a readable format.
    Show {
        /// Credential name, id, or fuzzy query. Supports prefixes: env:, pro:, cf:.
        /// Without prefix, searches across all fields.
        query: String,
        /// Show all credential fields, not just the secret.
        #[arg(long)]
        full: bool,
    },
    /// Remove a credential.
    Rm {
        /// Credential name or id.
        query: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Run a command with credentials injected as env vars.
    ///
    /// Example: devcred inject --env prod -- npm publish
    Inject {
        /// Filter by environment.
        #[arg(long)]
        env: Option<String>,
        /// Only inject credentials matching these names/env-vars (comma-separated).
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Disable secret redaction in the child process's stdout/stderr.
        /// By default, injected secret values are replaced with [REDACTED].
        #[arg(long = "no-redact")]
        no_redact: bool,
        /// Command and args after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage access tokens for restricted (basic) permission.
    ///
    /// A basic token allows read-only operations (list, copy, inject, show
    /// non-masked fields) but blocks editing, adding, deleting, and revealing
    /// masked fields. Useful for giving an AI agent limited vault access.
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },
    /// Launch the TUI (default when no subcommand is given).
    Tui,
}

#[derive(Subcommand, Debug)]
pub enum TokenAction {
    /// Create a new access token. Requires the master password.
    Create {
        /// Human-readable label for this token (e.g. "agent-readonly").
        #[arg(long)]
        label: Option<String>,
    },
    /// List all stored tokens (token strings are not shown).
    List,
    /// Revoke a token by id or label. Requires the master password.
    Revoke {
        /// Token id (numeric) or label.
        query: String,
    },
}

/// Entry point dispatched from `main`.
pub fn run(cli: Cli) -> Result<()> {
    let vault_path = cli.vault.clone().unwrap_or_else(|| {
        db::default_vault_path().expect("could not resolve default vault path")
    });

    match cli.command {
        None | Some(Command::Tui) => {
            let vault = open_vault(&vault_path)?;
            crate::tui::run(vault, vault_path).context("TUI session")?;
            Ok(())
        }
        Some(Command::Init) => init_vault(&vault_path),
        Some(Command::Add {
            name,
            secret,
            kind,
            env,
            project,
            env_var,
            notes,
            fields,
        }) => add_credential(&vault_path, name, secret, kind, env, project, env_var, notes, fields),
        Some(Command::List {
            env,
            project,
            query,
            reveal,
        }) => list_credentials(&vault_path, env, project, query, reveal),
        Some(Command::Copy {
            query,
            clear_after,
        }) => copy_credential(&vault_path, &query, clear_after),
        Some(Command::Show { query, full }) => show_credential(&vault_path, &query, full),
        Some(Command::Rm { query, yes }) => remove_credential(&vault_path, &query, yes),
        Some(Command::Token { action }) => handle_token(&vault_path, action),
        Some(Command::Inject { env, only, no_redact, args }) => {
            let vault = open_vault(&vault_path)?;
            if args.is_empty() {
                bail!("`inject` requires a command after `--`, e.g. `devcred inject -- npm publish`");
            }
            let (cmd, rest) = args.split_first().expect("non-empty");
            let code = injector::run(&vault, env.as_deref(), &only, cmd, rest, !no_redact)?;
            std::process::exit(code);
        }
    }
}

fn prompt_password(confirm: bool) -> Result<String> {
    // Allow non-interactive use (e.g. batch imports) via the environment.
    if let Ok(pw) = std::env::var("DEVCRED_PASSWORD") {
        if !pw.is_empty() {
            return Ok(pw);
        }
    }
    let pw = rpassword::prompt_password("Master password: ")?;
    if pw.is_empty() {
        bail!("master password cannot be empty");
    }
    if confirm {
        let pw2 = rpassword::prompt_password("Confirm master password: ")?;
        if pw != pw2 {
            bail!("passwords do not match");
        }
    }
    Ok(pw)
}

fn open_vault(path: &PathBuf) -> Result<Vault> {
    if !path.exists() {
        bail!(
            "vault not found at {}. Run `devcred init` first.",
            path.display()
        );
    }
    let pw = prompt_password(false)?;
    Vault::open(path, &pw).context("opening vault — wrong password?")
}

fn init_vault(path: &PathBuf) -> Result<()> {
    if path.exists() {
        print!("A vault already exists at {}. Overwrite? [y/N] ", path.display());
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        if !buf.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
        // Remove the main db file plus SQLite WAL/SHM sidecar files so
        // stale data from a previous vault doesn't leak into the new one.
        std::fs::remove_file(path).ok();
        let mut wal = path.as_os_str().to_os_string();
        wal.push("-wal");
        std::fs::remove_file(&wal).ok();
        let mut shm = path.as_os_str().to_os_string();
        shm.push("-shm");
        std::fs::remove_file(&shm).ok();
    }
    let pw = prompt_password(true)?;
    let vault = Vault::open(path, &pw).context("creating vault")?;
    println!("Vault initialized at {} ({} credentials).", path.display(), vault.count()?);
    Ok(())
}

fn add_credential(
    path: &PathBuf,
    name: Option<String>,
    secret: Option<String>,
    kind: Option<String>,
    env: Option<String>,
    project: Option<String>,
    env_var: Option<String>,
    notes: Option<String>,
    fields: Vec<String>,
) -> Result<()> {
    let vault = open_vault(path)?;

    let name = match name {
        Some(n) => n,
        None => prompt("Name (e.g. github-personal): ")?,
    };
    if name.trim().is_empty() {
        bail!("name is required");
    }

    let secret = match secret {
        Some(s) => s,
        None => {
            rpassword::prompt_password("Secret (paste the token): ")?
        }
    };
    let detection = credential::detect(&secret);
    if !detection.valid {
        eprintln!("Warning: {}", detection.note);
    }
    // A `--kind` override wins over auto-detection (and supports custom names).
    let kind = match kind {
        Some(k) => db::parse_kind(&k),
        None => detection.kind,
    };

    // When the master password comes from the environment we're in
    // non-interactive (scripted) mode: default missing optional fields to
    // empty instead of prompting, so batch imports don't stall.
    let non_interactive = std::env::var("DEVCRED_PASSWORD").is_ok();
    let env = match env {
        Some(e) => e,
        None if non_interactive => String::new(),
        None => prompt("Environment [optional, e.g. prod]: ")?,
    };
    let project = match project {
        Some(p) => p,
        None if non_interactive => String::new(),
        None => prompt("Project [optional, e.g. web-app]: ")?,
    };
    let env_var = match env_var {
        Some(e) => e,
        None if non_interactive => kind.env_var().to_string(),
        None => {
            let suggested = kind.env_var();
            let input = prompt(&format!("Env var [{suggested}]: "))?;
            if input.trim().is_empty() {
                suggested.to_string()
            } else {
                input
            }
        }
    };
    let notes = match notes {
        Some(n) => n,
        None if non_interactive => String::new(),
        None => prompt("Notes [optional]: ")?,
    };

    // Parse custom fields: each entry is "key=value" or "key=value:masked".
    let custom: Vec<(String, String, bool)> = fields
        .iter()
        .filter_map(|raw| parse_field(raw))
        .collect();

    let id = vault.add(
        &name,
        kind.clone(),
        env.trim(),
        project.trim(),
        &secret,
        env_var.trim(),
        &notes,
    )?;
    if !custom.is_empty() {
        vault.set_custom_fields(id, &custom)?;
    }
    println!(
        "Added credential #{}: {} ({}, env={}){}",
        id,
        name,
        kind.label(),
        if env.trim().is_empty() { "(none)" } else { env.trim() },
        if custom.is_empty() {
            String::new()
        } else {
            format!(", {} custom field(s)", custom.len())
        }
    );
    Ok(())
}

/// Parse a `--field` value: "key=value" or "key=value:masked".
/// Returns `(key, value, masked)`, or `None` if the format is invalid.
fn parse_field(raw: &str) -> Option<(String, String, bool)> {
    let eq = raw.find('=')?;
    let key = raw[..eq].trim().to_string();
    if key.is_empty() {
        return None;
    }
    let rest = &raw[eq + 1..];
    // Check for a trailing ":masked" suffix.
    let (value, masked) = if let Some(stripped) = rest.strip_suffix(":masked") {
        (stripped.to_string(), true)
    } else {
        (rest.to_string(), false)
    };
    Some((key, value, masked))
}

fn list_credentials(
    path: &PathBuf,
    env: Option<String>,
    project: Option<String>,
    query: Option<String>,
    reveal: bool,
) -> Result<()> {
    let vault = open_vault(path)?;
    if reveal {
        if !vault.permission().is_full() {
            bail!("permission denied: --reveal requires the master password");
        }
        // Re-confirm master password before dumping plaintext secrets.
        eprintln!("WARNING: revealing secrets in plaintext.");
        let confirm = rpassword::prompt_password("Re-enter master password to confirm: ")?;
        if !vault.verify_password(&confirm) {
            bail!("master password mismatch — revealing secrets refused");
        }
    }
    let records = vault.list(env.as_deref(), project.as_deref())?;
    let records = if let Some(q) = query {
        fuzzy_filter(&vault, &records, &q)?
    } else {
        records
    };
    if records.is_empty() {
        println!("(no credentials)");
        return Ok(());
    }
    println!(
        "{:<4} {:<24} {:<12} {:<12} {:<24} {}",
        "ID", "NAME", "KIND", "ENV", "PROJECT", "ENV_VAR"
    );
    for r in &records {
        let secret_field = if reveal {
            let d = vault.decrypt(r)?;
            format!("  {}", mask_or_reveal(&d.secret))
        } else {
            String::new()
        };
        println!(
            "{:<4} {:<24} {:<12} {:<12} {:<24} {}{}",
            r.id,
            truncate(&r.name, 24),
            r.kind,
            truncate(&r.environment, 12),
            truncate(&r.project, 24),
            r.env_var,
            secret_field
        );
    }
    Ok(())
}

fn copy_credential(path: &PathBuf, query: &str, clear_after: u64) -> Result<()> {
    let vault = open_vault(path)?;
    let rec = lookup(&vault, query)?.context("no matching credential")?;
    let dec = vault.decrypt(&rec)?;
    clipboard::copy_and_clear_after(&dec.secret, clear_after)?;
    eprintln!(
        "Copied `{}` to clipboard. Auto-clearing in {}s.",
        dec.name, clear_after
    );
    Ok(())
}

fn show_credential(path: &PathBuf, query: &str, full: bool) -> Result<()> {
    let vault = open_vault(path)?;
    // Try exact lookup first (by id or name), then fall back to fuzzy search.
    let rec = match lookup(&vault, query)? {
        Some(r) => r,
        None => {
            let matches = fuzzy_filter(&vault, &vault.list(None, None)?, query)?;
            if let Some(best) = matches.into_iter().next() {
                best
            } else {
                bail!("no matching credential for `{}`", query);
            }
        }
    };
    let dec = vault.decrypt(&rec)?;
    let is_full = vault.permission().is_full();

    if full && !is_full {
        bail!("permission denied: --full requires the master password");
    }

    if !full {
        // Pipe-friendly mode: secret to stdout, custom fields to stderr.
        // In basic mode, masked custom fields are omitted entirely.
        if !dec.custom_fields.is_empty() {
            eprintln!("Custom fields for `{}`:", dec.name);
            for cf in &dec.custom_fields {
                if cf.masked && !is_full {
                    continue; // Hide masked fields in basic mode.
                }
                let val = if cf.masked {
                    "•".repeat(cf.value.chars().count().min(16))
                } else {
                    cf.value.clone()
                };
                eprintln!("  {}: {}{}", cf.key, val, if cf.masked { " (masked)" } else { "" });
            }
        }
        print!("{}", dec.secret);
        return Ok(());
    }

    // Full mode: print all fields in a readable format to stdout.
    println!("ID:          {}", dec.id);
    println!("Name:        {}", dec.name);
    println!("Kind:        {}", dec.kind.label());
    println!("Environment: {}", if dec.environment.is_empty() { "(none)" } else { &dec.environment });
    println!("Project:     {}", if dec.project.is_empty() { "(none)" } else { &dec.project });
    println!("Env Var:     {}", dec.env_var);
    println!("Secret:      {}", dec.secret);
    if !dec.notes.is_empty() {
        println!("Notes:       {}", dec.notes);
    }
    if !dec.custom_fields.is_empty() {
        println!();
        println!("Custom Fields:");
        for cf in &dec.custom_fields {
            let masked_tag = if cf.masked { " (masked)" } else { "" };
            println!("  {:<16}: {}{}", cf.key, cf.value, masked_tag);
        }
    }
    println!();
    println!("Created:     {}", fmt_ts_cli(dec.created_at));
    println!("Updated:     {}", fmt_ts_cli(dec.updated_at));
    Ok(())
}

/// Format a Unix timestamp as a human-readable UTC string.
fn fmt_ts_cli(ts: i64) -> String {
    use chrono::DateTime;
    match DateTime::from_timestamp(ts, 0) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => ts.to_string(),
    }
}

fn remove_credential(path: &PathBuf, query: &str, yes: bool) -> Result<()> {
    let vault = open_vault(path)?;
    let rec = lookup(&vault, query)?.context("no matching credential")?;
    if !yes {
        print!("Delete `{}` ({} env={})? [y/N] ", rec.name, rec.kind, rec.environment);
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        if !buf.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }
    if vault.delete(rec.id)? {
        println!("Deleted `{}`.", rec.name);
    } else {
        bail!("nothing deleted");
    }
    Ok(())
}

/// Handle `devcred token` subcommands.
fn handle_token(path: &PathBuf, action: TokenAction) -> Result<()> {
    let vault = open_vault(path)?;
    match action {
        TokenAction::Create { label } => {
            if !vault.permission().is_full() {
                bail!("permission denied: creating tokens requires the master password");
            }
            let label = label.unwrap_or_else(|| {
                let count = vault.list_tokens().map(|t| t.len()).unwrap_or(0);
                format!("token-{}", count + 1)
            });
            let token = vault.create_token(&label)?;
            // Copy to clipboard with auto-clear.
            match crate::clipboard::copy_and_clear_after(&token, 60) {
                Ok(_) => {
                    println!("Token (label: {}) copied to clipboard.", label);
                    eprintln!("Clipboard will auto-clear in 60 seconds.");
                    eprintln!("Use it as the DEVCRED_PASSWORD for read-only access.");
                }
                Err(_) => {
                    // Clipboard failed — fall back to printing.
                    println!("Token (label: {}):", label);
                    println!();
                    println!("  {}", token);
                    println!();
                    eprintln!("WARNING: clipboard unavailable — token printed to stdout.");
                    eprintln!("Store this token securely — it will not be shown again.");
                }
            }
        }
        TokenAction::List => {
            let tokens = vault.list_tokens()?;
            if tokens.is_empty() {
                println!("(no tokens)");
                return Ok(());
            }
            println!(
                "{:<4} {:<16} {:<20} {}",
                "ID", "LABEL", "TOKEN_ID", "CREATED"
            );
            for t in &tokens {
                println!(
                    "{:<4} {:<16} {:<20} {}",
                    t.id,
                    truncate(&t.label, 16),
                    &t.token_id[..8.min(t.token_id.len())],
                    fmt_ts_cli(t.created_at),
                );
            }
        }
        TokenAction::Revoke { query } => {
            if vault.revoke_token(&query)? {
                println!("Token `{}` revoked.", query);
            } else {
                bail!("no token matching `{}`", query);
            }
        }
    }
    Ok(())
}

/// Parsed query with optional field prefix.
enum QueryScope {
    /// Search across all fields (name, project, env, kind, env_var, custom field keys).
    All,
    /// Search only in the environment field.
    Env,
    /// Search only in the project field.
    Project,
    /// Search only in custom field keys.
    CustomField,
}

/// Parse a query string, detecting prefixes like "env:", "pro:", "cf:".
fn parse_query(query: &str) -> (QueryScope, &str) {
    let lower = query.to_lowercase();
    if lower.starts_with("env:") {
        (QueryScope::Env, &query[4..])
    } else if lower.starts_with("pro:") {
        (QueryScope::Project, &query[4..])
    } else if lower.starts_with("cf:") {
        (QueryScope::CustomField, &query[3..])
    } else {
        (QueryScope::All, query)
    }
}

/// Filter credential records using fuzzy matching with optional field-scoped queries.
/// Returns records sorted by match score (best first), then by name alphabetically.
fn fuzzy_filter(
    vault: &Vault,
    records: &[db::CredentialRecord],
    query: &str,
) -> Result<Vec<db::CredentialRecord>> {
    let (scope, term) = parse_query(query);
    if term.trim().is_empty() {
        return Ok(records.to_vec());
    }
    let matcher = SkimMatcherV2::default();
    let term_lower = term.trim().to_lowercase();

    // Pre-load custom field keys for each record.
    let cf_keys: std::collections::HashMap<i64, String> = records
        .iter()
        .filter_map(|r| {
            let keys = vault.custom_field_keys(r.id).unwrap_or_default();
            if keys.is_empty() {
                None
            } else {
                Some((r.id, keys.join(" ").to_lowercase()))
            }
        })
        .collect();

    let mut scored: Vec<(i64, db::CredentialRecord)> = records
        .iter()
        .filter_map(|r| {
            let hay = match scope {
                QueryScope::All => {
                    let mut h = format!(
                        "{} {} {} {} {}",
                        r.name, r.kind, r.environment, r.project, r.env_var
                    )
                    .to_lowercase();
                    if let Some(cf) = cf_keys.get(&r.id) {
                        h.push(' ');
                        h.push_str(cf);
                    }
                    h
                }
                QueryScope::Env => r.environment.to_lowercase(),
                QueryScope::Project => r.project.to_lowercase(),
                QueryScope::CustomField => cf_keys.get(&r.id).cloned().unwrap_or_default(),
            };
            matcher.fuzzy_match(&hay, &term_lower).map(|score| (score, r.clone()))
        })
        .collect();

    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
    });

    Ok(scored.into_iter().map(|(_, r)| r).collect())
}

/// Look up a credential by id (numeric) or name (case-insensitive).
fn lookup(vault: &Vault, query: &str) -> Result<Option<db::CredentialRecord>> {
    if let Ok(id) = query.parse::<i64>() {
        if let Some(r) = vault.get(id)? {
            return Ok(Some(r));
        }
    }
    Ok(vault.find_by_name(query)?)
}

fn prompt(msg: &str) -> Result<String> {
    print!("{msg}");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf.trim_end().to_string())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

fn mask_or_reveal(s: &str) -> String {
    s.to_string()
}
