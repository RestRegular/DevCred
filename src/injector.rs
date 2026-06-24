//! Inject credentials as environment variables when running subcommands.
//!
//! Example: `devcred inject --env prod -- npm publish` unlocks the vault,
//! loads every credential tagged with environment `prod`, exports each one
//! under its `env_var` name (e.g. `NODE_AUTH_TOKEN`), and runs `npm publish`
//! as a child process — so secrets never touch `.env` files or disk.
//!
//! ## Output redaction
//!
//! By default, the child process's stdout and stderr are scanned for any
//! injected secret values and replaced with `[REDACTED]` before being passed
//! through to the terminal. This prevents trivial leaks like
//! `inject -- cmd /c echo %GITHUB_TOKEN%`.
//!
//! Only the main secret and **masked** custom-field values are redacted.
//! Plain (non-masked) custom fields are treated as metadata (e.g. usernames,
//! regions, ports) and pass through unredacted.
//!
//! A determined attacker can still extract secrets character-by-character,
//! so this is a defense-in-depth measure, not a complete sandbox. Use
//! `--no-redact` to disable filtering (e.g. for debugging pipe issues).

use crate::db::Vault;
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;

/// Run `cmd` with `args`, injecting every credential from `environment` as an
/// env var. If `names` is non-empty, only credentials whose name is in the set
/// are injected (handy for `--only GITHUB_TOKEN,STRIPE_SECRET_KEY`).
///
/// When `redact` is true (the default), the child's stdout/stderr are filtered
/// to replace any injected secret values with `[REDACTED]`.
///
/// Returns the child's exit code.
pub fn run(
    vault: &Vault,
    environment: Option<&str>,
    names: &[String],
    cmd: &str,
    args: &[String],
    redact: bool,
) -> Result<i32> {
    if cmd.is_empty() {
        bail!("no command given after `--`");
    }

    let records = vault.list(environment, None).context("listing credentials")?;
    let name_filter: Option<std::collections::HashSet<&str>> = if names.is_empty() {
        None
    } else {
        Some(names.iter().map(String::as_str).collect())
    };

    let mut command = Command::new(cmd);
    command.args(args);

    // Collect all secret values for redaction (sorted longest-first so that
    // if one secret is a substring of another, the longer one is replaced
    // first).
    let mut secrets: Vec<String> = Vec::new();
    let mut injected = 0usize;
    for rec in &records {
        if let Some(filter) = &name_filter {
            if !filter.contains(rec.name.as_str()) && !filter.contains(rec.env_var.as_str()) {
                continue;
            }
        }
        if rec.env_var.is_empty() {
            continue;
        }
        let dec = vault.decrypt(rec)?;
        command.env(rec.env_var.clone(), &dec.secret);
        // Collect the secret and any masked custom-field values for redaction.
        // Plain (non-masked) custom fields are metadata, not secrets.
        if !dec.secret.is_empty() {
            secrets.push(dec.secret);
        }
        for cf in &dec.custom_fields {
            if cf.masked && !cf.value.is_empty() {
                secrets.push(cf.value.clone());
            }
        }
        injected += 1;
    }

    if injected == 0 {
        bail!(
            "no credentials matched (env={:?}, names={:?}) — nothing to inject",
            environment,
            names
        );
    }

    if redact && !secrets.is_empty() {
        // Sort longest-first so substrings don't prevent longer matches.
        secrets.sort_by(|a, b| b.len().cmp(&a.len()));

        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        // stdin stays inherited for interactive commands.

        let mut child = command
            .spawn()
            .with_context(|| format!("spawning `{cmd}`"))?;

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let secrets_out = secrets.clone();
        let stdout_handle = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            let out = std::io::stdout();
            let mut out = out.lock();
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        let filtered = redact_line(&l, &secrets_out);
                        let _ = writeln!(out, "{}", filtered);
                        let _ = out.flush();
                    }
                    Err(_) => break,
                }
            }
        });

        let secrets_err = secrets.clone();
        let stderr_handle = thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let err = std::io::stderr();
            let mut err = err.lock();
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        let filtered = redact_line(&l, &secrets_err);
                        let _ = writeln!(err, "{}", filtered);
                        let _ = err.flush();
                    }
                    Err(_) => break,
                }
            }
        });

        // Wait for the child to finish.
        let status = child.wait().with_context(|| format!("waiting for `{cmd}`"))?;
        // Ensure all filtered output has been flushed.
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();
        Ok(status.code().unwrap_or(1))
    } else {
        // No redaction: inherit stdio directly (original behavior).
        let status = command
            .status()
            .with_context(|| format!("spawning `{cmd}`"))?;
        Ok(status.code().unwrap_or(1))
    }
}

/// Replace every occurrence of any secret in `line` with `[REDACTED]`.
fn redact_line(line: &str, secrets: &[String]) -> String {
    let mut result = line.to_string();
    for secret in secrets {
        if secret.len() >= 4 && result.contains(secret.as_str()) {
            result = result.replace(secret.as_str(), "[REDACTED]");
        }
    }
    result
}
