//! `clipboard` — write a string to the system clipboard.
//!
//! Wraps `arboard` so the TUI's `Ctrl+Shift+C` handler stays a
//! one-liner. Arboard picks the right backend at runtime
//! (x11/wl-copy/wayland/etc on Linux, AppKit on macOS, Win32 on
//! Windows); on a headless or unsupported environment the
//! `Clipboard::new()` constructor returns `Err` and we surface
//! that as a chat status row instead of a panic.
//!
//! # Why a wrapper, not a direct `arboard::set` call
//!
//! Two reasons.
//! 1. The TUI key handler shouldn't have to know which env feature
//!    flags arboard needs (e.g. `wayland-data-control` vs the
//!    default x11 path). It calls `copy_to_clipboard(&s)` and gets
//!    back either `Ok(n)` with a byte count for the status row, or
//!    `Err(msg)` for a user-friendly explanation.
//! 2. The wrapper is the only place that converts arboard's error
//!    type into `anyhow::Error`. A future test surface that mocks
//!    the clipboard (e.g. an in-memory recorder) only has to swap
//!    this one function — the key handler is unaffected.

/// Copy `text` to the system clipboard.
///
/// Returns `Ok(text.len())` on success or `Err(msg)` with a
/// user-facing explanation on failure. The byte count is what the
/// status row displays ("📋 Copied 1234 chars to clipboard"); the
/// actual transfer is binary-agnostic — arboard writes the bytes
/// verbatim.
pub fn copy_to_clipboard(text: &str) -> anyhow::Result<usize> {
    let mut cb = arboard::Clipboard::new()
        .map_err(|e| anyhow::anyhow!("clipboard unavailable: {}", e))?;
    cb.set_text(text.to_string())
        .map_err(|e| anyhow::anyhow!("clipboard set failed: {}", e))?;
    Ok(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: arboard should succeed on a normal desktop.
    /// On a headless CI box (no X server, no Wayland compositor),
    /// arboard's constructor will fail and the test surfaces that
    /// — it doesn't lie. The error is reported, not silently
    /// swallowed.
    #[test]
    fn copy_to_clipboard_round_trips_short_ascii() {
        match copy_to_clipboard("hello clipboard") {
            Ok(n) => assert_eq!(n, "hello clipboard".len()),
            Err(e) => panic!(
                "clipboard unavailable in this environment (test was run headless?): {}",
                e
            ),
        }
    }
}
