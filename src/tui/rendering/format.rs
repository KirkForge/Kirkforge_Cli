//! Pure display-formatting helpers for the status bar.
//!
//! Extracted from `mod.rs`: duration, token-count, and budget-indicator
//! formatting. These take primitives in and return `String` / `Color`
//! out — no ratatui frame state — so they're unit-testable in isolation.

use crate::tui::theme::Theme;
use ratatui::style::Color;

/// Format a duration for display.
pub fn format_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else if secs < 3600.0 {
        format!("{:.0}m {:.0}s", secs / 60.0, secs % 60.0)
    } else {
        format!("{:.0}h {:.0}m", secs / 3600.0, (secs % 3600.0) / 60.0)
    }
}

/// Token budget formatting: "12.4K / 128K"
pub fn format_token_count(tokens: usize) -> String {
    if tokens < 1000 {
        tokens.to_string()
    } else {
        format!("{:.1}K", tokens as f64 / 1000.0)
    }
}

/// Format a token-budget indicator for the status bar.
///
/// Returns a `(text, color)` pair. The text is one of:
/// - `""` (empty) when `max == 0` or `used == 0` AND `max == 0` — i.e. the
///   caller hasn't supplied a budget. Caller should fall back to the
///   plain `↑N` display in that case.
/// - `"<used>"` (e.g. `"12.4K"`) when `max == 0` — same fallback,
///   but we still return the formatted used count so the caller can
///   use it without recomputing.
/// - `"<used>/<max> (P%)"` (e.g. `"12.4K/128K (10%)"`) when both
///   `used > 0` and `max > 0`.
///
/// The color is the budget-pressure threshold:
/// - `Green`  — `< 50%`  (comfortable)
/// - `Yellow` — `50–80%` (getting tight, consider `/compact`)
/// - `Red`    — `80–95%` (compact now)
/// - `LightRed` (Rgb 255, 100, 100) — `> 95%` (the B1.x layers are kicking in)
///
/// Why this lives here and not in `status.rs`: the helper is pure
/// (string + color out, no ratatui types) so it's trivially unit-testable
/// without a frame buffer. The status widget just consumes the output.
pub fn format_budget_indicator(used: usize, max: usize, theme: &Theme) -> (String, Color) {
    match budget_pct(used, max) {
        None => {
            // No budget known (model not connected yet, or model has
            // 0 max_context_tokens in its config). Return the plain
            // used-count so the caller can fall back to `↑N`.
            (format_token_count(used), theme.budget_neutral)
        }
        Some(pct) => {
            let color = if pct < 50 {
                theme.budget_comfortable
            } else if pct < 80 {
                theme.budget_tight
            } else if pct < 95 {
                theme.budget_high
            } else {
                theme.budget_cliff
            };

            let text = format!(
                "{}/{} ({}%)",
                format_token_count(used),
                format_token_count(max),
                pct
            );

            (text, color)
        }
    }
}

/// Compute the context-budget percentage used, in `0..=100`.
///
/// Returns `None` if `max == 0` (no model connected yet, or the
/// model reports `0` for `max_context_tokens`). Caller should
/// treat `None` as "no recommendation can be made."
///
/// Uses `saturating_mul` + `checked_div` so an absurd `used` value
/// can't overflow and a zero `max` is the only failure mode.
pub fn budget_pct(used: usize, max: usize) -> Option<u8> {
    if max == 0 {
        return None;
    }
    // saturating_mul prevents overflow; checked_div returns Some
    // because we just guarded max > 0. Clamp to 100 because used
    // can exceed max (e.g. after a tool cap that pushed past the
    // model's advertised context).
    Some((used.saturating_mul(100) / max).min(100) as u8)
}
