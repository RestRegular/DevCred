<div align="center">

# DevCred

**Local, encrypted credential manager for developers**

A fast, terminal-based vault for API keys, tokens, and 2FA recovery codes — built in Rust with AES-256-GCM encryption and an intuitive TUI.

[![Rust](https://img.shields.io/badge/Rust-1.70+-orange?logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey)]()

</div>

---

## Why DevCred?

Developers juggle dozens of credentials — GitHub tokens, AWS keys, npm publish tokens, PyPI API keys, 2FA recovery codes. Storing them in plaintext files or browser password managers is risky and inconvenient. DevCred keeps everything in a single encrypted local vault with a keyboard-driven TUI, CLI scripting support, and zero network dependencies.

## Features

### Security
- **AES-256-GCM** encryption for all secrets and custom field values
- **Argon2id** key derivation (64 MiB / 3 iterations) resists brute-force attacks
- **Master password** never stored — verified via encrypted probe on unlock
- **Auto-clearing clipboard** — copied secrets are wiped after a configurable timeout
- **Zero network** — everything stays local, no telemetry, no cloud sync

### TUI (Terminal User Interface)
- **Keyboard-driven** — navigate, search, edit, and copy without touching the mouse
- **Fuzzy search** — quickly find credentials by name, kind, or project
- **Category sidebar** — filter by credential kind (GitHub, AWS, npm, PyPI, custom, etc.)
- **Environment & project filters** — narrow down by `prod` / `staging` / project tags
- **Detail view** — inspect all fields with selective copy and masked reveal
- **Custom fields** — add arbitrary key-value pairs with per-field masked display
- **Inline editing** — create and edit credentials directly in the TUI
- **Horizontal scrolling** — long values stay editable with cursor-visible scrolling
- **Copy feedback** — green flash on the selected row confirms a successful copy

### CLI (Scripting)
- **Non-interactive mode** — set `DEVCRED_PASSWORD` env var for CI/CD pipelines
- **`inject` subcommand** — run commands with credentials injected as env vars
- **Batch import** — add credentials from JSON/TXT files via shell scripts
- **Pipe-friendly** — `show` outputs secrets to stdout, metadata to stderr

## Installation

### From source

```bash
git clone https://github.com/RestRegular/DevCred.git
cd DevCred
cargo build --release
# Binary: target/release/devcred
```

> **Tip:** Always use `--release` for daily use. The debug build is 10–50x slower due to Argon2id's memory-hard KDF.

### Add to PATH

```bash
# Linux/macOS
cp target/release/devcred /usr/local/bin/

# Windows (PowerShell)
Copy-Item target\release\devcred.exe "$env:USERPROFILE\.cargo\bin\"
```

## Quick Start

```bash
# 1. Initialize the vault (sets master password)
devcred init

# 2. Launch the TUI
devcred

# 3. Add a credential via CLI
devcred add --name github-personal --secret ghp_xxxx --kind github --env-var GITHUB_TOKEN

# 4. Copy a secret to clipboard (auto-clears after 15s)
devcred copy github-personal

# 5. Inject credentials into a command
devcred inject --env prod -- npm publish
```

## CLI Reference

| Command | Description |
|---------|-------------|
| `devcred init` | Initialize a new encrypted vault |
| `devcred` | Launch the TUI (default) |
| `devcred add [flags]` | Add a credential |
| `devcred list [--env E] [--project P] [--reveal]` | List credentials |
| `devcred copy <query> [--clear-after N]` | Copy secret to clipboard |
| `devcred show <query>` | Print secret to stdout |
| `devcred rm <query> [--yes]` | Remove a credential |
| `devcred inject [--env E] [--only N1,N2] -- <cmd>` | Run command with injected env vars |

### `add` flags

```
--name <NAME>          Display name (e.g. "github-personal")
--secret <SECRET>      Raw secret (prompted if omitted)
--kind <KIND>          Override auto-detection (e.g. "github", "aws", or custom)
--env <ENV>            Environment tag (e.g. "prod", "staging")
--project <PROJECT>    Project group (e.g. "web-app")
--env-var <VAR>        Env var name for `inject` (e.g. "GITHUB_TOKEN")
--notes <NOTES>        Free-form notes
--field <K=V[:masked]> Custom field (repeatable). ":masked" hides value in TUI
```

### Environment variables

| Variable | Description |
|----------|-------------|
| `DEVCRED_PASSWORD` | Master password (enables non-interactive mode) |
| `DEVCRED_VAULT` | Path to vault file (default: `~/.config/devcred/vault.db`) |

## TUI Keybindings

### List view

| Key | Action |
|-----|--------|
| `Tab` | Switch between Categories sidebar and credential list |
| `↑` `↓` / `k` `j` | Navigate |
| `/` | Search |
| `c` / `↲` | Copy secret to clipboard |
| `i` / `↲` | Open detail view |
| `e` | Filter by environment |
| `p` | Filter by project |
| `n` | New credential |
| `r` | Edit credential |
| `d` | Delete credential |
| `?` | Help |
| `q` / `Esc` | Quit |

### Detail view

| Key | Action |
|-----|--------|
| `Tab` / `↑` `↓` | Select field |
| `c` / `↲` | Copy selected field (defaults to Secret) |
| `⇧1` `⇧2` ... | Quick-copy field by number |
| `s` | Reveal masked values |
| `PgUp` / `PgDn` | Scroll (when content overflows) |
| `e` | Edit credential |
| `Esc` | Back to list |

### Form view (Add / Edit)

| Key | Action |
|-----|--------|
| `Tab` / `↑` `↓` | Navigate fields |
| `←` `→` | Cycle credential kind |
| `↲` | Save / Add custom field |
| `⌫` on `[masked]` | Remove custom field |
| `Esc` | Cancel |

## Custom Fields

Custom fields let you attach arbitrary key-value data to any credential. They're only displayed in the detail view and can be individually masked.

```bash
# CLI: add with custom fields
devcred add \
  --name aws-prod \
  --secret AKIA... \
  --kind aws \
  --field "region=us-east-1" \
  --field "mfa_serial=arn:aws:iam::123:mfa/user:masked" \
  --field "account_id=123456789012"
```

In the TUI, press `n` to create a new credential, then use `[+ Add custom field]` to add rows. Toggle `[✓ masked]` / `[✗ plain]` to control whether each value is hidden in the detail view.

## Security Model

```
┌─────────────┐     Argon2id      ┌──────────────┐
│ Master      │ ──────────────▶   │ 256-bit key  │
│ Password    │   64 MiB / 3 iter │ (never saved)│
└─────────────┘                   └──────┬───────┘
                                         │
                                         ▼
┌─────────────┐     AES-256-GCM  ┌──────────────┐
│ Secret      │ ◀──────────────▶ │ Encrypted    │
│ Plaintext   │   per-value nonce│ Blob (SQLite)│
└─────────────┘                  └──────────────┘
```

- The master password is processed through **Argon2id** (memory-hard KDF) to derive a 256-bit encryption key.
- Each secret and custom field value is encrypted with **AES-256-GCM** using a unique random nonce.
- The derived key is **never persisted** — it exists only in memory while DevCred is running.
- On vault open, a **probe value** (encrypted with the master key) verifies the password before granting access.
- Clipboard auto-clears after **15 seconds** (configurable via `--clear-after`).

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust (edition 2021) |
| TUI | [ratatui](https://crates.io/crates/ratatui) 0.29 + [crossterm](https://crates.io/crates/crossterm) 0.28 |
| Database | [rusqlite](https://crates.io/crates/rusqlite) 0.32 (bundled SQLite) |
| Encryption | [aes-gcm](https://crates.io/crates/aes-gcm) 0.10 + [argon2](https://crates.io/crates/argon2) 0.5 |
| Clipboard | [arboard](https://crates.io/crates/arboard) 3 |
| CLI | [clap](https://crates.io/crates/clap) 4.5 (derive) |
| Search | [fuzzy-matcher](https://crates.io/crates/fuzzy-matcher) 0.3 (SkimMatcherV2) |

## Project Structure

```
src/
├── main.rs          # Entry point
├── cli.rs           # CLI subcommands (init, add, list, copy, inject, ...)
├── crypto.rs        # AES-256-GCM + Argon2id key derivation
├── db.rs            # SQLite vault (encrypted storage, custom fields)
├── credential.rs    # Credential kind detection (GitHub, AWS, npm, ...)
├── clipboard.rs     # Clipboard copy with timed auto-clear
├── injector.rs      # Env var injection for subcommands
└── tui/
    ├── mod.rs       # TUI state, event handling, keybindings
    └── ui.rs        # Rendering (list, detail, form, filters)
```

## License

[MIT](LICENSE)
