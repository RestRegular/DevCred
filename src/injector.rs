//! Inject credentials as environment variables when running subcommands.
//!
//! Example: `devcred inject --env prod -- npm publish` unlocks the vault,
//! loads every credential tagged with environment `prod`, exports each one
//! under its `env_var` name (e.g. `NODE_AUTH_TOKEN`), and runs `npm publish`
//! as a child process — so secrets never touch `.env` files or disk.

use crate::db::Vault;
use anyhow::{Context, Result, bail};
use std::process::Command;

/// Run `cmd` with `args`, injecting every credential from `environment` as an
/// env var. If `names` is non-empty, only credentials whose name is in the set
/// are injected (handy for `--only GITHUB_TOKEN,STRIPE_SECRET_KEY`).
///
/// The child inherits stdin/stdout/stderr. Returns the child's exit status.
pub fn run(
    vault: &Vault,
    environment: Option<&str>,
    names: &[String],
    cmd: &str,
    args: &[String],
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
        command.env(rec.env_var.clone(), dec.secret);
        injected += 1;
    }

    if injected == 0 {
        bail!(
            "no credentials matched (env={:?}, names={:?}) — nothing to inject",
            environment,
            names
        );
    }

    let status = command
        .status()
        .with_context(|| format!("spawning `{cmd}`"))?;
    Ok(status.code().unwrap_or(1))
}
