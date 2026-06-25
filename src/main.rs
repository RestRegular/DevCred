//! DevCred — local, encrypted credential manager for developers.
//!
//! Run `devcred` with no arguments to launch the TUI, or use a subcommand:
//! `init`, `add`, `list`, `copy`, `show`, `rm`, `inject`, `tui`.

mod cli;
mod clipboard;
mod credential;
mod crypto;
mod db;
mod injector;
mod transfer;
mod tui;

use clap::Parser;
use cli::{Cli, run};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run(cli)
}
