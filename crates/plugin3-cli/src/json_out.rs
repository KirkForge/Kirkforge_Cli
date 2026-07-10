//! Safe JSON serialisation helpers for CLI command output.
//!
//! ponytail: command handlers historically used
//! `serde_json::to_string_pretty(...).unwrap()` on `serde_json::Value`
//! and plain-map types. These are infallible in practice, but the
//! `.unwrap()` still opens a panic surface. The helpers here close it
//! by printing a parseable JSON envelope on the failure path so a
//! host or wrapper script never receives a non-JSON response.

use serde::Serialize;

/// Pretty-print `value` to stdout.
///
/// If serialisation fails, print a parseable JSON error object instead
/// of panicking. This is the default for `--json` command output.
pub fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => println!("{{\"error\":\"JSON serialisation failed: {e}\"}}"),
    }
}

/// Pretty-print `value` to stdout, falling back to `fallback` text on
/// serialisation failure.
///
/// Use this when the caller expects a specific empty envelope (e.g.
/// `{}` or `[]`) and an `"error"` object would break a downstream
/// parser that only knows the happy-path shape.
pub fn print_json_or<T: Serialize>(value: &T, fallback: &str) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("plugin3: JSON serialisation failed: {e}");
            println!("{fallback}");
        }
    }
}
