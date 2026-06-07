//! `@-mention` resolution and rendering.
//!
//! A line containing `@<path>` tokens is "augmented" before being
//! sent to the model: each token is replaced with the file's
//! contents, formatted as a fenced code block. This gives the user a
//! "review this file" gesture without forcing them to use the
//! model's tool calls.
//!
//! Examples:
//!
//!   @src/main.rs                          inline the file (minified)
//!   @src/main.rs:raw                      inline the file verbatim
//!   @src/main.rs:10-50                    inline lines 10–50 (1-indexed,
//!                                         inclusive on both ends)
//!   @~/notes.md                           tilde expansion supported
//!   @src/lib.rs:10-50:raw                 range + verbatim
//!   multiple @<path> tokens in one input  all expanded
//!
//! The file is read at submit time, NOT when the model asks for it.
//! If the file is huge, it is minified (default) and capped at
//! `MENTION_MAX_BYTES`. Missing/denied/unreadable paths are NOT
//! errors from the user's perspective — the prompt still goes
//! through, and the model sees a short
//! `[could not read: <reason>]` placeholder so it can react. We
//! strip the `@<path>` tokens from the user-facing display copy
//! (they would just look like noise in the chat log).
//!
//! Pure helpers throughout — the I/O happens in `expand_mentions`,
//! everything else is byte/string surgery. This makes the parsing
//! edge cases trivially unit-testable.

use crate::session::access::PathGuard;
use crate::shared::minify::minify_source;
use std::path::Path;

/// Per-mention byte cap. Matches the read_file budget so a single
/// @-mention cannot blow the model's context window by itself.
pub const MENTION_MAX_BYTES: usize = 50_000;

/// Number of bytes a single mention occupies in the rendered prompt
/// when the source file is too large to fit. We always include some
/// head + tail + a marker so the model sees both the call site and
/// the result/error (same pattern as the tool output cap).
const MENTION_HEAD_BYTES: usize = 30_000;
const MENTION_TAIL_BYTES: usize = 15_000;

/// What the user actually asked for. The path may be relative
/// (resolved against the project root at expand time) or absolute.
/// `raw` suppresses the default minification.
#[derive(Debug, Clone, PartialEq)]
pub struct MentionSpec {
    pub path: String,
    /// 1-indexed, inclusive on both ends. `None` = whole file.
    pub range: Option<(usize, usize)>,
    pub raw: bool,
}

/// A mention that has been resolved against the filesystem.
#[derive(Debug, Clone, PartialEq)]
pub struct MentionExpansion {
    pub spec: MentionSpec,
    pub content: String,
    pub status: MentionStatus,
}

/// Outcome of the file read. `Ok` carries the number of bytes the
/// model actually sees (post-minify, post-truncate) and whether the
/// file was minified or truncated.
#[derive(Debug, Clone, PartialEq)]
pub enum MentionStatus {
    Ok {
        bytes: usize,
        minified: bool,
        truncated: bool,
    },
    NotFound,
    Denied(String),
    IoError(String),
    InvalidRange(String),
}

/// Scan `input` for `@<path>[:<range>][:raw]` tokens.
///
/// A token starts with `@` and continues until the next whitespace
/// (or end of string). Inside the token, the path component is
/// everything up to the first `:`; if no `:` is present, the whole
/// thing after `@` is the path. The path may not contain `:`, so
/// `~`-expanded home dirs are fine but `C:\...` Windows paths will
/// be cut at the colon. We accept that limitation — the TUI runs
/// on Linux/macOS in practice.
///
/// Edge cases handled:
///
/// - `@` alone (no path) — NOT a mention, kept as literal text
/// - `@@foo` (double-`@`) — kept as literal text; only single `@` starts a mention
/// - `@path with spaces` — first whitespace ends the mention
/// - `@path:` (empty range) — kept as literal path (no range)
/// - `@path:abc` (non-numeric range) — kept as literal path
#[derive(Debug, Clone, PartialEq)]
pub struct MentionToken {
    pub spec: MentionSpec,
    pub start: usize,
    pub end: usize,
}

pub fn parse_mentions(input: &str) -> Vec<MentionToken> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // Reject `@@` (double-@ — only single @ starts a mention).
        if i + 1 < bytes.len() && bytes[i + 1] == b'@' {
            i += 2;
            continue;
        }
        // Reject `@` at end of input or followed by whitespace.
        let after = i + 1;
        if after >= bytes.len() || bytes[after].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Find end of token — next whitespace.
        let mut end = after;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        // Parse the slice input[after..end] as `path[:range][:raw]`.
        let raw = &input[after..end];
        if let Some(spec) = parse_mention_spec(raw) {
            out.push(MentionToken {
                spec,
                start: i,
                end,
            });
        }
        i = end;
    }
    out
}

/// Parse a `path[:range][:raw]` string into a `MentionSpec`. Returns
/// `None` if the path is empty or the range syntax is malformed
/// (in which case the caller should NOT treat the original text as
/// a mention — we want the user to see it literally).
fn parse_mention_spec(raw: &str) -> Option<MentionSpec> {
    // Strip a trailing `:raw` first.
    let (raw_stripped, raw) = match raw.strip_suffix(":raw") {
        Some(rest) => (rest, true),
        None => (raw, false),
    };
    // Now look for a range. The range uses `-` as the separator and
    // is the FIRST `:...` segment if present.
    let (path_part, range) = match raw_stripped.find(':') {
        Some(idx) => {
            let (p, r) = raw_stripped.split_at(idx);
            // r starts with `:` — strip it and parse the range body.
            let range_body = &r[1..];
            let parsed = parse_range(range_body);
            match parsed {
                Some(rng) => (p, Some(rng)),
                None => {
                    // Malformed range — bail out and let the user
                    // see the literal text. This is important for
                    // @-mentions inside prose that happen to
                    // contain a colon (e.g. `@see RFC 1234: details`).
                    return None;
                }
            }
        }
        None => (raw_stripped, None),
    };
    if path_part.is_empty() {
        return None;
    }
    Some(MentionSpec {
        path: path_part.to_string(),
        range,
        raw,
    })
}

/// Parse a `START-END` range string. `START` and `END` are positive
/// integers; `END >= START`. Returns `None` for malformed input.
fn parse_range(body: &str) -> Option<(usize, usize)> {
    let dash = body.find('-')?;
    let (a, b) = body.split_at(dash);
    let b = &b[1..]; // strip the `-`
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let start: usize = a.parse().ok()?;
    let end: usize = b.parse().ok()?;
    if end < start {
        return None;
    }
    Some((start, end))
}

/// Remove the raw `@<path>...` tokens from `input` and collapse the
/// whitespace around them. Returns the cleaned text. The relative
/// ordering of the non-mention text is preserved.
///
/// We do not delete the original text — the user typed it and we
/// want to show them what the model actually received. Instead, we
/// replace each mention with an empty string, then collapse runs of
/// whitespace that adjoin the deletion.
pub fn strip_mentions(input: &str, mentions: &[MentionToken]) -> String {
    if mentions.is_empty() {
        return input.to_string();
    }
    // Sort by start (parse_mentions already returns them in order, but
    // we don't want to rely on that).
    let mut sorted: Vec<&MentionToken> = mentions.iter().collect();
    sorted.sort_by_key(|m| m.start);
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    for m in sorted {
        // Append everything between cursor and m.start, then collapse
        // any trailing whitespace into a single space.
        if m.start > cursor {
            out.push_str(&input[cursor..m.start]);
        }
        // Skip the mention and any whitespace immediately after it.
        let mut new_cursor = m.end;
        while new_cursor < input.len() && input.as_bytes()[new_cursor].is_ascii_whitespace() {
            new_cursor += 1;
        }
        cursor = new_cursor;
        // If we still have non-whitespace text after the mention
        // (in practice this doesn't happen because the mention
        // ends at whitespace, but defensive), leave a single space
        // so words don't run together.
        if cursor < input.len() && cursor > m.end {
            out.push(' ');
        }
    }
    if cursor < input.len() {
        out.push_str(&input[cursor..]);
    }
    out
}

/// Read the files for the given mention parses and return one
/// expansion per parse. Uses `PathGuard` for the same path-safety
/// checks the model's `read_file` tool would — a path that's denied
/// for the model is also denied for the user-driven mention.
///
/// Behaviour matches the model's tool semantics:
/// - `read_file` denials (`Denied` verdict) → `MentionStatus::Denied`
/// - missing file → `MentionStatus::NotFound`
/// - I/O error → `MentionStatus::IoError`
/// - malformed range → `MentionStatus::InvalidRange`
/// - success → `MentionStatus::Ok` with bytes/minified/truncated flags
pub fn expand_mentions(mentions: &[MentionToken], path_guard: &PathGuard) -> Vec<MentionExpansion> {
    mentions.iter().map(|m| expand_one(m, path_guard)).collect()
}

fn expand_one(m: &MentionToken, path_guard: &PathGuard) -> MentionExpansion {
    // Tilde expansion — same convention as read_file.
    let expanded = shellexpand::tilde(&m.spec.path);
    let path = Path::new(expanded.as_ref());

    // PathGuard::check_read denies a missing path with a "Path does
    // not exist" reason. We want the user to see a clean `NotFound`
    // for the common "I typed the wrong path" case rather than the
    // raw guard message, so check existence up front and short-circuit.
    if !path.exists() {
        return MentionExpansion {
            spec: m.spec.clone(),
            content: String::new(),
            status: MentionStatus::NotFound,
        };
    }

    // Path safety check. The user's intent is "I want the model to
    // see this file", which is a read.
    let resolved = match path_guard.check_read(path) {
        crate::session::access::GuardVerdict::Allowed(p) => p,
        crate::session::access::GuardVerdict::Denied(reason) => {
            return MentionExpansion {
                spec: m.spec.clone(),
                content: String::new(),
                status: MentionStatus::Denied(reason),
            };
        }
    };

    // Read the file.
    let raw = match std::fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return MentionExpansion {
                spec: m.spec.clone(),
                content: String::new(),
                status: MentionStatus::NotFound,
            };
        }
        Err(e) => {
            return MentionExpansion {
                spec: m.spec.clone(),
                content: String::new(),
                status: MentionStatus::IoError(e.to_string()),
            };
        }
    };

    // Apply range filter (1-indexed, inclusive on both ends).
    let ranged: String = if let Some((start, end)) = m.spec.range {
        // We need to validate the range against the actual line count.
        let lines: Vec<&str> = raw.lines().collect();
        if start == 0 || start > lines.len() {
            return MentionExpansion {
                spec: m.spec.clone(),
                content: String::new(),
                status: MentionStatus::InvalidRange(format!(
                    "start line {} is out of range (file has {} lines)",
                    start,
                    lines.len()
                )),
            };
        }
        let end = end.min(lines.len());
        lines[(start - 1)..end].join("\n")
    } else {
        // Strip a single trailing newline if present so that
        // @-mentioning a typical text file (which ends in `\n`)
        // produces a clean prompt without a phantom blank line at
        // the end. The model's read_file tool returns content
        // verbatim, but @-mentions are inlined into prose, so the
        // trim is the user-friendly default.
        let trimmed = raw.strip_suffix('\n').unwrap_or(&raw);
        trimmed.to_string()
    };

    // Apply minification unless :raw was specified.
    let minified = !m.spec.raw;
    let content = if m.spec.raw {
        ranged
    } else {
        minify_source(&resolved, &ranged)
    };

    // Truncate if still too big — keep head + marker + tail so the
    // model sees both the call site and the result/error (same
    // pattern as the tool output cap).
    let (final_content, truncated) = truncate_to_cap(&content);

    MentionExpansion {
        spec: m.spec.clone(),
        content: final_content,
        status: MentionStatus::Ok {
            bytes: content.len(),
            minified,
            truncated,
        },
    }
}

fn truncate_to_cap(content: &str) -> (String, bool) {
    if content.len() <= MENTION_MAX_BYTES {
        return (content.to_string(), false);
    }
    // We need the tail to come from the END of the original content,
    // not the middle. Take head from the start, tail from the end.
    let head_end = MENTION_HEAD_BYTES.min(content.len());
    let tail_start = content.len().saturating_sub(MENTION_TAIL_BYTES);
    let head = &content[..head_end];
    let tail = &content[tail_start..];
    let marker = format!(
        "\n... [truncated, {} bytes total — showing first {} + last {}] ...\n",
        content.len(),
        MENTION_HEAD_BYTES,
        MENTION_TAIL_BYTES
    );
    let mut out = String::with_capacity(head.len() + marker.len() + tail.len());
    out.push_str(head);
    out.push_str(&marker);
    out.push_str(tail);
    (out, true)
}

/// Build the inlined block that gets appended to the model's prompt.
/// One fenced code block per mention, in input order. Failures
/// (denied/missing/etc.) are rendered as short `> ` quoted placeholders
/// so the model can react to them in the same turn.
pub fn render_mentions_block(expansions: &[MentionExpansion]) -> String {
    if expansions.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nThe user shared the following files for context:\n");
    for e in expansions {
        let label = mention_label(e);
        match &e.status {
            MentionStatus::Ok {
                bytes,
                minified,
                truncated,
            } => {
                let mut flags = Vec::new();
                if *minified {
                    flags.push("minified");
                }
                if *truncated {
                    flags.push("truncated");
                }
                let annotation = if flags.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", flags.join(", "))
                };
                out.push_str(&format!(
                    "\n### `{}` — {} bytes{}\n```\n{}\n```\n",
                    label, bytes, annotation, e.content
                ));
            }
            MentionStatus::NotFound => {
                out.push_str(&format!(
                    "\n### `{}` — could not read: file not found\n",
                    label
                ));
            }
            MentionStatus::Denied(reason) => {
                out.push_str(&format!(
                    "\n### `{}` — could not read: denied ({})\n",
                    label, reason
                ));
            }
            MentionStatus::IoError(err) => {
                out.push_str(&format!(
                    "\n### `{}` — could not read: I/O error ({})\n",
                    label, err
                ));
            }
            MentionStatus::InvalidRange(reason) => {
                out.push_str(&format!(
                    "\n### `{}` — could not read: invalid range ({})\n",
                    label, reason
                ));
            }
        }
    }
    out
}

/// Human-readable label for the rendered block header. Shows the path
/// as typed plus a `:START-END` / `:raw` suffix if those were used.
fn mention_label(e: &MentionExpansion) -> String {
    let mut s = e.spec.path.clone();
    if let Some((start, end)) = e.spec.range {
        s.push_str(&format!(":{}-{}", start, end));
    }
    if e.spec.raw {
        s.push_str(":raw");
    }
    s
}

/// One-line system message for the TUI chat log describing what was
/// inlined. Tells the user "your @-mentions were resolved" with a
/// per-file status row. Pure formatter.
pub fn format_mention_status(expansions: &[MentionExpansion]) -> String {
    if expansions.is_empty() {
        return String::new();
    }
    let mut out = String::from("📎 @-mentions resolved:\n");
    for e in expansions {
        let label = mention_label(e);
        match &e.status {
            MentionStatus::Ok {
                bytes,
                minified,
                truncated,
            } => {
                let mut note = format!("{} bytes", bytes);
                if *minified {
                    note.push_str(", minified");
                }
                if *truncated {
                    note.push_str(", truncated to cap");
                }
                out.push_str(&format!("  ✓ `{}` — {}\n", label, note));
            }
            MentionStatus::NotFound => {
                out.push_str(&format!("  ✗ `{}` — not found\n", label));
            }
            MentionStatus::Denied(reason) => {
                out.push_str(&format!("  ✗ `{}` — denied ({})\n", label, reason));
            }
            MentionStatus::IoError(err) => {
                out.push_str(&format!("  ✗ `{}` — I/O error: {}\n", label, err));
            }
            MentionStatus::InvalidRange(reason) => {
                out.push_str(&format!("  ✗ `{}` — {}\n", label, reason));
            }
        }
    }
    out
}
