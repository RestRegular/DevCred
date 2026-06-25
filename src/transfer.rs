//! Import/export module for DevCred.
//!
//! Supports exporting credentials to JSON, CSV, and XLSX formats,
//! and importing from JSON, CSV, and TXT formats.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::db::DecryptedCredential;

// -- Data structures -----------------------------------------------

/// A single credential entry for serialization/deserialization.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct TransferEntry {
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub custom_fields: Vec<TransferCustomField>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct TransferCustomField {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub masked: bool,
}

/// Statistics from an import operation.
pub struct ImportStats {
    pub total: usize,
    pub imported: usize,
    pub skipped: usize,
    pub errors: usize,
}

impl std::fmt::Display for ImportStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Total: {}, Imported: {}, Skipped: {}, Errors: {}",
            self.total, self.imported, self.skipped, self.errors
        )
    }
}

// -- Format detection ----------------------------------------------

/// Detect format from file extension.
pub fn detect_format(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("json") => Some("json"),
        Some("csv") => Some("csv"),
        Some("xlsx") => Some("xlsx"),
        Some("txt") => Some("txt"),
        _ => None,
    }
}

// -- Export functions ----------------------------------------------

/// Convert a DecryptedCredential to a TransferEntry.
fn to_transfer_entry(cred: &DecryptedCredential, no_reveal: bool) -> TransferEntry {
    TransferEntry {
        name: cred.name.clone(),
        kind: crate::db::kind_label(&cred.kind),
        secret: if no_reveal {
            String::new()
        } else {
            cred.secret.clone()
        },
        env: if cred.environment.is_empty() {
            None
        } else {
            Some(cred.environment.clone())
        },
        project: if cred.project.is_empty() {
            None
        } else {
            Some(cred.project.clone())
        },
        env_var: if cred.env_var.is_empty() {
            None
        } else {
            Some(cred.env_var.clone())
        },
        notes: if cred.notes.is_empty() {
            None
        } else {
            Some(cred.notes.clone())
        },
        custom_fields: cred
            .custom_fields
            .iter()
            .map(|f| TransferCustomField {
                key: f.key.clone(),
                value: if no_reveal && f.masked {
                    String::new()
                } else {
                    f.value.clone()
                },
                masked: f.masked,
            })
            .collect(),
    }
}

/// Export credentials as JSON.
pub fn export_json(credentials: &[DecryptedCredential], no_reveal: bool) -> Result<String> {
    let entries: Vec<TransferEntry> = credentials
        .iter()
        .map(|c| to_transfer_entry(c, no_reveal))
        .collect();
    let json = serde_json::to_string_pretty(&entries).context("serializing to JSON")?;
    Ok(json)
}

/// Export credentials as CSV.
pub fn export_csv(credentials: &[DecryptedCredential], no_reveal: bool) -> Result<String> {
    let mut wtr = csv::Writer::from_writer(Vec::new());
    // Header
    wtr.write_record(&[
        "name",
        "kind",
        "secret",
        "env",
        "project",
        "env_var",
        "notes",
        "custom_fields",
    ])
    .context("writing CSV header")?;

    for cred in credentials {
        let entry = to_transfer_entry(cred, no_reveal);
        let cf_json = if entry.custom_fields.is_empty() {
            String::from("[]")
        } else {
            serde_json::to_string(&entry.custom_fields).unwrap_or_else(|_| "[]".to_string())
        };
        wtr.write_record(&[
            &entry.name,
            &entry.kind,
            &entry.secret,
            entry.env.as_deref().unwrap_or(""),
            entry.project.as_deref().unwrap_or(""),
            entry.env_var.as_deref().unwrap_or(""),
            entry.notes.as_deref().unwrap_or(""),
            &cf_json,
        ])
        .context("writing CSV row")?;
    }

    let data = wtr
        .into_inner()
        .context("finalizing CSV writer")?;
    Ok(String::from_utf8(data).context("CSV output is not valid UTF-8")?)
}

/// Export credentials as XLSX.
pub fn export_xlsx(
    credentials: &[DecryptedCredential],
    no_reveal: bool,
    path: &Path,
) -> Result<()> {
    use rust_xlsxwriter::{Format, Workbook};

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    worksheet.set_name("Credentials").ok();

    let header_fmt = Format::new().set_bold();
    let headers = [
        "name",
        "kind",
        "secret",
        "env",
        "project",
        "env_var",
        "notes",
        "custom_fields",
    ];
    for (col, header) in headers.iter().enumerate() {
        worksheet
            .write_string_with_format(0, col as u16, *header, &header_fmt)
            .context("writing XLSX header")?;
    }

    for (row, cred) in credentials.iter().enumerate() {
        let entry = to_transfer_entry(cred, no_reveal);
        let r = (row + 1) as u32;
        let cf_json = if entry.custom_fields.is_empty() {
            String::from("[]")
        } else {
            serde_json::to_string(&entry.custom_fields).unwrap_or_else(|_| "[]".to_string())
        };
        let cols = [
            entry.name.clone(),
            entry.kind.clone(),
            entry.secret.clone(),
            entry.env.clone().unwrap_or_default(),
            entry.project.clone().unwrap_or_default(),
            entry.env_var.clone().unwrap_or_default(),
            entry.notes.clone().unwrap_or_default(),
            cf_json,
        ];
        for (col, val) in cols.iter().enumerate() {
            worksheet
                .write_string(r, col as u16, val)
                .context("writing XLSX row")?;
        }
    }

    // Auto-fit columns
    worksheet.autofit();

    workbook
        .save(path)
        .with_context(|| format!("saving XLSX to {}", path.display()))?;
    Ok(())
}

// -- Import parsing functions --------------------------------------

/// Parse JSON array of TransferEntry.
pub fn parse_json(content: &str) -> Result<Vec<TransferEntry>> {
    let entries: Vec<TransferEntry> =
        serde_json::from_str(content).context("parsing JSON input")?;
    Ok(entries)
}

/// Parse CSV with headers: name,kind,secret,env,project,env_var,notes,custom_fields.
pub fn parse_csv(content: &str) -> Result<Vec<TransferEntry>> {
    let mut rdr = csv::Reader::from_reader(content.as_bytes());
    let headers: Vec<String> = rdr
        .headers()?
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut entries = Vec::new();
    for result in rdr.records() {
        let record = result.context("reading CSV record")?;
        let mut entry = TransferEntry {
            name: String::new(),
            kind: String::new(),
            secret: String::new(),
            env: None,
            project: None,
            env_var: None,
            notes: None,
            custom_fields: Vec::new(),
        };
        for (i, field) in record.iter().enumerate() {
            if i >= headers.len() {
                break;
            }
            let val = field.to_string();
            match headers[i].as_str() {
                "name" => entry.name = val,
                "kind" => entry.kind = val,
                "secret" => entry.secret = val,
                "env" => {
                    if !val.is_empty() {
                        entry.env = Some(val);
                    }
                }
                "project" => {
                    if !val.is_empty() {
                        entry.project = Some(val);
                    }
                }
                "env_var" => {
                    if !val.is_empty() {
                        entry.env_var = Some(val);
                    }
                }
                "notes" => {
                    if !val.is_empty() {
                        entry.notes = Some(val);
                    }
                }
                "custom_fields" => {
                    if !val.is_empty() && val != "[]" {
                        entry.custom_fields =
                            serde_json::from_str(&val).unwrap_or_default();
                    }
                }
                _ => {}
            }
        }
        if !entry.name.is_empty() {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Parse TXT format: name=secret or pipe-delimited fields.
/// Lines starting with # and empty lines are ignored.
///
/// Supported pipe-delimited formats (fields separated by |):
/// - 2 fields: name|secret
/// - 4 fields: name|kind|env|secret
/// - 7 fields: name|kind|env|project|env_var|secret|notes
pub fn parse_txt(content: &str) -> Result<Vec<TransferEntry>> {
    let mut entries = Vec::new();
    for (line_num, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.contains('|') {
            // Pipe-delimited format
            let parts: Vec<&str> = line.split('|').collect();
            let entry = match parts.len() {
                2 => TransferEntry {
                    name: parts[0].trim().to_string(),
                    kind: String::new(),
                    secret: parts[1].trim().to_string(),
                    env: None,
                    project: None,
                    env_var: None,
                    notes: None,
                    custom_fields: Vec::new(),
                },
                4 => TransferEntry {
                    name: parts[0].trim().to_string(),
                    kind: parts[1].trim().to_string(),
                    secret: parts[3].trim().to_string(),
                    env: Some(parts[2].trim().to_string()),
                    project: None,
                    env_var: None,
                    notes: None,
                    custom_fields: Vec::new(),
                },
                7 => TransferEntry {
                    name: parts[0].trim().to_string(),
                    kind: parts[1].trim().to_string(),
                    secret: parts[5].trim().to_string(),
                    env: Some(parts[2].trim().to_string()),
                    project: Some(parts[3].trim().to_string()),
                    env_var: Some(parts[4].trim().to_string()),
                    notes: Some(parts[6].trim().to_string()),
                    custom_fields: Vec::new(),
                },
                _ => {
                    bail!(
                        "line {}: expected 2, 4, or 7 pipe-delimited fields, got {}",
                        line_num + 1,
                        parts.len()
                    );
                }
            };
            entries.push(entry);
        } else if let Some(eq_pos) = line.find('=') {
            // name=secret format
            let name = line[..eq_pos].trim().to_string();
            let secret = line[eq_pos + 1..].trim().to_string();
            if name.is_empty() {
                bail!("line {}: empty name before = sign", line_num + 1);
            }
            entries.push(TransferEntry {
                name,
                kind: String::new(),
                secret,
                env: None,
                project: None,
                env_var: None,
                notes: None,
                custom_fields: Vec::new(),
            });
        } else {
            bail!(
                "line {}: expected name=secret or pipe-delimited fields, got: {}",
                line_num + 1,
                line
            );
        }
    }
    Ok(entries)
}
