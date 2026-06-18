//! Clipboard operations with timed auto-clear.
//!
//! Copying a secret sets the system clipboard and spawns a watcher thread that
//! clears it after a configurable delay. If another copy happens before the
//! timer fires, the previous watcher exits without clearing so the new secret
//! gets its own full window.

use anyhow::{Context, Result};
use arboard::Clipboard;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Default auto-clear delay: 30 seconds.
pub const DEFAULT_CLEAR_SECS: u64 = 30;

/// Monotonic token bumped on every copy so stale watchers can self-cancel.
static CLEAR_TOKEN: AtomicU64 = AtomicU64::new(0);

/// Copy `text` to the clipboard and schedule it to be cleared after `secs`.
///
/// Returns the token associated with this copy (mostly useful for tests).
pub fn copy_and_clear_after(text: &str, secs: u64) -> Result<u64> {
    let mut cb = Clipboard::new().context("opening clipboard")?;
    cb.set_text(text)
        .map_err(|e| anyhow::anyhow!("clipboard set failed: {e}"))?;

    let token = CLEAR_TOKEN.fetch_add(1, Ordering::SeqCst) + 1;
    let watch_token = token;
    let wait = Duration::from_secs(secs.max(1));

    thread::spawn(move || {
        thread::sleep(wait);
        // Only clear if no newer copy has bumped the token.
        if CLEAR_TOKEN.load(Ordering::SeqCst) == watch_token {
            if let Ok(mut cb) = Clipboard::new() {
                let _ = cb.set_text("");
            }
        }
    });

    Ok(token)
}
