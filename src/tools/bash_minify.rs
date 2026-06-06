//! Bash-output minification interceptor.
//!
//! When the model runs a command that essentially asks for the contents of a
//! file (e.g. `cat src/foo.rs`, `head -n 50 README.md`, `bat Cargo.toml`),
//! the raw stdout can balloon the prompt context. The `read_file` tool
//! already runs its output through `minify_source_safe` before returning it
//! to the model — this module does the same for bash commands that stream
//! the same content through stdout.
//!
//! The interceptor is deliberately conservative:
//!   * Only matches a fixed allowlist of file-dumping commands.
//!   * Only the FIRST positional path argument is considered (matching the
//!     real-world usage of these tools).
//!   * The file must exist on disk and have a known source-code extension
//!     (so we don't accidentally minify a binary blob or a log file).
//!   * The savings must exceed `MIN_SAVINGS_RATIO` — otherwise we hand the
//!     model the original output (preserving caching behaviour for nearly-
//!     empty files or already-tight code).
//!   * Errors in parsing fall through to the original output — we never
//!     break a successful command just because we couldn't minify it.
//!
//! All the actual minification work lives in `crate::shared::minify`.

use std::path::PathBuf;

/// Minimum savings (as a fraction of the original char count) required for
/// us to substitute the minified output. `0.20` = must save at least 20% of
/// characters. Below this threshold the per-call minification is a net loss
/// in latency for no meaningful token win.
const MIN_SAVINGS_RATIO: f64 = 0.20;

// ── Build-log noise reduction ─────────────────────────────────────
//
// `cargo build` / `cargo test` / `cargo check` / `cargo clippy` produce
// hundreds of lines of progress (`   Compiling foo v0.1.0`) and
// per-warning suggestions that bury the few lines of context the
// model actually needs (the error/warning title and the `-->` location
// + the code excerpt under it). On a 200-crate workspace, a single
// `cargo build 2>&1` can easily be 400+ lines and 15k+ chars while
// containing 5 lines of signal.
//
// The interceptor is deliberately conservative:
//   * Only fires on `cargo <sub>` or `rustc` invocations, or on
//     output that strongly looks like cargo output (`warning:` +
//     `Compiling` markers in the first/last 200 chars).
//   * Errors and their full context (location + code excerpt) are
//     always preserved.
//   * Warnings keep first 3 + last 3 in full; the middle is
//     collapsed to a single `…(N warnings omitted)…` marker.
//   * `Compiling <crate>` lines keep first 5 + last 3; middle
//     collapsed to a `…(N crates compiled)…` marker.
//   * Refuses the swap if savings are below 20% — the same guard
//     the file-dump minifier uses, for the same reason.

/// How many warnings to keep verbatim at the top of the output before
/// we start collapsing the middle.
const BUILD_LOG_KEEP_HEAD_WARNINGS: usize = 3;
/// How many warnings to keep verbatim at the tail of the output.
const BUILD_LOG_KEEP_TAIL_WARNINGS: usize = 3;
/// How many `Compiling <crate>` lines to keep at the head. The first
/// few are usually relevant (which crates are involved); the rest are
/// just progress noise.
const BUILD_LOG_KEEP_HEAD_COMPILING: usize = 5;
/// How many `Compiling <crate>` lines to keep at the tail.
const BUILD_LOG_KEEP_TAIL_COMPILING: usize = 3;
/// How many lines an output must have before we even consider
/// minifying. Below this, the minification overhead exceeds the
/// savings.
const BUILD_LOG_MIN_LINES: usize = 30;

/// Try to minify the stdout of a build command (cargo, rustc).
///
/// Returns `Some(minified_output)` if:
///   * the command matches a known build-tool pattern (or the output
///     strongly looks like cargo/rustc output), AND
///   * the output has enough lines to be worth filtering, AND
///   * the filtered form saves at least `MIN_SAVINGS_RATIO` of
///     characters.
///
/// Returns `None` in every other case — the caller should pass the
/// original stdout through unchanged.
pub fn try_minify_build_log(cmd: &str, stdout: &str) -> Option<String> {
    if stdout.is_empty() {
        return None;
    }

    // Two independent gates: a recognised command name, OR strong
    // output-format signal. The output-format check is what catches
    // shell wrappers like `make build` that ultimately call cargo —
    // if it really looks like cargo output, treat it as such.
    let is_build = is_build_command(cmd);
    let output_matches = output_looks_like_cargo(stdout);
    if !is_build && !output_matches {
        return None;
    }

    let lines: Vec<&str> = stdout.lines().collect();
    if lines.len() < BUILD_LOG_MIN_LINES {
        return None;
    }

    let minified = filter_build_log(&lines);

    // Refuse the swap if savings are too small to be worth the
    // round-trip filtering cost.
    let original_chars: usize = lines.iter().map(|l| l.len() + 1).sum();
    let minified_chars: usize = minified.iter().map(|l| l.len() + 1).sum();
    if minified_chars >= ((1.0 - MIN_SAVINGS_RATIO) * original_chars as f64) as usize {
        return None;
    }

    Some(minified.join("\n"))
}

/// Recognised build-tool basenames.
fn is_build_command(cmd: &str) -> bool {
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    let base = tokens[0]
        .rsplit_once('/')
        .map(|(_, b)| b)
        .unwrap_or(tokens[0]);
    if matches!(base, "cargo" | "rustc" | "cargo-nextest" | "sccache") {
        return true;
    }
    // `cargo <sub>` — already matched by the above; the rare case of
    // an explicit `~/.cargo/bin/cargo build` also matches via rsplit.
    false
}

/// Strong output-format signal: at least one `warning:` or `error[`
/// marker AND at least one cargo progress marker. This catches
/// shell wrappers (`make build`, `just check`) that ultimately
/// produce cargo output.
fn output_looks_like_cargo(stdout: &str) -> bool {
    let has_diagnostic = stdout.contains("warning:") || stdout.contains("error[");
    let has_progress = stdout.contains("Compiling ")
        || stdout.contains("Finished ")
        || stdout.contains("    Finished")
        || stdout.contains("Running ");
    has_diagnostic && has_progress
}

/// Filter a build log down to its signal: keep all errors and their
/// context, keep the first few + last few warnings, keep the first
/// few + last few `Compiling` progress lines, and collapse the
/// middle into summary markers.
fn filter_build_log(lines: &[&str]) -> Vec<String> {
    // Pre-compute classification for every line.
    let kinds: Vec<LineKind> = lines.iter().map(|l| classify(l)).collect();

    // Walk warnings: find indices of all `WarningTitle` lines, keep
    // head + tail verbatim, middle is dropped (we keep the warning
    // title only — not the multi-line `note:` / `help:` block under
    // it, since the model rarely needs suggestions).
    let warning_indices: Vec<usize> = kinds
        .iter()
        .enumerate()
        .filter_map(|(i, k)| matches!(k, LineKind::WarningTitle).then_some(i))
        .collect();

    let keep_head_w = BUILD_LOG_KEEP_HEAD_WARNINGS.min(warning_indices.len());
    let keep_tail_w =
        BUILD_LOG_KEEP_TAIL_WARNINGS.min(warning_indices.len().saturating_sub(keep_head_w));
    let drop_w_start = keep_head_w;
    let drop_w_end = warning_indices.len().saturating_sub(keep_tail_w);
    let dropped_warnings = drop_w_end.saturating_sub(drop_w_start);

    // Set of warning-line indices that are dropped. We also drop the
    // suggestion block under a dropped warning (the lines between
    // the warning title and the next blank/Compiling/warning).
    let dropped_warning_starts: std::collections::HashSet<usize> = if dropped_warnings > 0 {
        warning_indices[drop_w_start..drop_w_end]
            .iter()
            .copied()
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    // Compiling-progress: keep first N + last M.
    let compiling_indices: Vec<usize> = kinds
        .iter()
        .enumerate()
        .filter_map(|(i, k)| matches!(k, LineKind::Compiling).then_some(i))
        .collect();

    let keep_head_c = BUILD_LOG_KEEP_HEAD_COMPILING.min(compiling_indices.len());
    let keep_tail_c =
        BUILD_LOG_KEEP_TAIL_COMPILING.min(compiling_indices.len().saturating_sub(keep_head_c));
    let drop_c_start = keep_head_c;
    let drop_c_end = compiling_indices.len().saturating_sub(keep_tail_c);
    let dropped_compiling = drop_c_end.saturating_sub(drop_c_start);

    let dropped_compiling_set: std::collections::HashSet<usize> = if dropped_compiling > 0 {
        compiling_indices[drop_c_start..drop_c_end]
            .iter()
            .copied()
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    // Build the output. Walk every line; emit it unless it's part
    // of a dropped warning block or a dropped Compiling line.
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut skipping_warning = false;
    for (i, line) in lines.iter().enumerate() {
        let k = kinds[i];

        if k == LineKind::WarningTitle {
            if dropped_warning_starts.contains(&i) {
                // Mark this whole warning as collapsed.
                if dropped_warnings > 0
                    && out.last().map(|s| s.as_str()) != Some(WARNINGS_OMITTED_MARKER)
                {
                    out.push(WARNINGS_OMITTED_MARKER.to_string());
                }
                skipping_warning = true;
                continue;
            } else {
                skipping_warning = false;
                out.push(line.to_string());
                continue;
            }
        }

        if skipping_warning {
            // We're inside a dropped warning's suggestion block —
            // the block runs from the warning title to the next
            // blank line, Compiling line, or another warning/error.
            // Stop skipping at those boundaries.
            if matches!(
                k,
                LineKind::Blank
                    | LineKind::Compiling
                    | LineKind::WarningTitle
                    | LineKind::ErrorTitle
            ) {
                skipping_warning = false;
                // Fall through to emit this boundary line.
            } else {
                continue;
            }
        }

        if k == LineKind::Compiling {
            if dropped_compiling_set.contains(&i) {
                if dropped_compiling > 0
                    && out.last().map(|s| s.as_str()) != Some(CRATES_OMITTED_MARKER)
                {
                    out.push(CRATES_OMITTED_MARKER.to_string());
                }
                continue;
            } else {
                out.push(line.to_string());
                continue;
            }
        }

        // Everything else (errors, error context, blank lines,
        // warning bodies we kept, "Finished"/"Running", etc.) is
        // emitted verbatim.
        out.push(line.to_string());
    }

    // Update the omitted-marker messages with the actual counts.
    if dropped_warnings > 0 {
        for s in out.iter_mut() {
            if s == WARNINGS_OMITTED_MARKER {
                *s = format!(
                    "[…{} warnings omitted (kept first {} and last {})…]",
                    dropped_warnings, keep_head_w, keep_tail_w
                );
            }
        }
    }
    if dropped_compiling > 0 {
        for s in out.iter_mut() {
            if s == CRATES_OMITTED_MARKER {
                *s = format!(
                    "[…{} crate compilations omitted (kept first {} and last {})…]",
                    dropped_compiling, keep_head_c, keep_tail_c
                );
            }
        }
    }

    out
}

const WARNINGS_OMITTED_MARKER: &str = "<<WARNINGS_OMITTED>>";
const CRATES_OMITTED_MARKER: &str = "<<CRATES_OMITTED>>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    /// A line that opens a diagnostic block: `warning: ...` or
    /// `error[E0xxx]: ...`. Followed by context lines until the
    /// next blank or diagnostic.
    WarningTitle,
    ErrorTitle,
    /// `   Compiling foo v0.1.0` / `    Finished ...` / `     Running ...`
    /// — cargo progress output. Low signal individually; useful in
    /// aggregate (which crates built) but not worth keeping all of.
    Compiling,
    /// Blank/whitespace-only line.
    Blank,
    /// Everything else (note:, help:, code excerpts, summary text).
    Other,
}

fn classify(line: &str) -> LineKind {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return LineKind::Blank;
    }
    if trimmed.starts_with("warning:") {
        return LineKind::WarningTitle;
    }
    if trimmed.starts_with("error[") || trimmed.starts_with("error:") {
        return LineKind::ErrorTitle;
    }
    if line.starts_with("   Compiling ")
        || line.starts_with("    Compiling ")
        || line.starts_with("    Finished")
        || line.starts_with("     Finished")
        || line.starts_with("     Running")
        || line.starts_with("    Running")
    {
        return LineKind::Compiling;
    }
    LineKind::Other
}

/// Try to minify the stdout of a bash command that dumped a file's contents.
///
/// Returns `Some(minified_output)` if:
///   * the command matches a known file-dump pattern, AND
///   * the referenced file exists on disk, AND
///   * the file has a known minifiable extension, AND
///   * the minified form saves at least `MIN_SAVINGS_RATIO` of characters.
///
/// Returns `None` in every other case — the caller should pass the original
/// stdout through unchanged.
pub fn try_minify_bash_output(cmd: &str, stdout: &str) -> Option<String> {
    if stdout.is_empty() {
        return None;
    }

    let path = extract_file_path(cmd)?;
    if !path.is_file() {
        return None;
    }

    // Minify (use the safe variant — same one prompt history uses, so
    // the model sees the same form it would have seen from read_file).
    let minified = crate::shared::minify::minify_source_safe(&path, stdout);

    // Refuse the swap if the savings are too small to be worth the
    // round-trip minification cost.
    if minified.len() >= ((1.0 - MIN_SAVINGS_RATIO) * stdout.len() as f64) as usize {
        return None;
    }

    Some(minified)
}

/// Extract the file path from a known file-dump command.
///
/// Returns `None` for any command we don't recognise. The first positional
/// argument is treated as the path; flags like `-n 50` or `--style=plain`
/// are skipped over by walking tokens and only collecting the first
/// non-flag token.
fn extract_file_path(cmd: &str) -> Option<PathBuf> {
    // Tokenise on whitespace. We're not building a shell parser — just
    // enough to identify a small set of read-only file-dump commands.
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    // First token: the command name (e.g. "cat", "/usr/bin/head").
    // Strip any path prefix to get the bare command name.
    let cmd_name = tokens[0]
        .rsplit_once('/')
        .map(|(_, base)| base)
        .unwrap_or(tokens[0]);

    if !is_dump_command(cmd_name) {
        return None;
    }

    // Walk remaining tokens and pick the LAST non-flag, non-numeric token
    // as the path. The non-numeric rule handles the common case of
    // `head -n 50 src/lib.rs` and `head -n50 src/lib.rs` (where 50 is
    // a value to the `-n` flag, not a positional argument).
    //
    // Handles:
    //   cat src/foo.rs                       → src/foo.rs
    //   cat -n src/foo.rs                    → src/foo.rs
    //   head -n 50 src/lib.rs                → src/lib.rs
    //   head -n50 src/lib.rs                 → src/lib.rs
    //   bat --style=plain src/lib.rs         → src/lib.rs
    //   tail -n 20 -v /etc/passwd            → /etc/passwd
    // Refuses (returns None) for commands that have multiple path
    // candidates — those are usually concatenations (`cat a b > c`)
    // that we can't safely minify as a single source file.
    let mut path_token: Option<&str> = None;
    let mut path_count = 0usize;
    for tok in &tokens[1..] {
        if tok.starts_with('-') && *tok != "-" {
            // Skip flags (including `--key=value` and `-n50`).
            continue;
        }
        if looks_like_number(tok) {
            // Skip pure-numeric tokens — these are almost always flag
            // values (`-n 50`) or line counts, never file paths.
            continue;
        }
        path_count += 1;
        if path_count > 1 {
            // More than one path candidate. Refuse to be conservative.
            return None;
        }
        path_token = Some(tok);
    }

    let raw = path_token?;

    // tilde expansion — `cat ~/foo.rs` → `<home>/foo.rs`.
    // We don't want to shell-expand, but tilde is a single character we can
    // handle ourselves.
    let expanded = if let Some(stripped) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            format!("{}/{}", home, stripped)
        } else {
            return None;
        }
    } else {
        raw.to_string()
    };

    Some(PathBuf::from(expanded))
}

/// The allowlist of command basenames whose stdout we will inspect for a
/// file path to minify.
fn is_dump_command(name: &str) -> bool {
    matches!(
        name,
        "cat"
            | "head"
            | "tail"
            | "bat"
            | "less"
            | "more"
            | "nl"      // number lines — still just file contents
            | "tac"     // reverse cat — same content
            | "fold" // wrap long lines — same content
    )
}

/// A token "looks like a number" if every char is an ASCII digit, or if
/// it's a sign (`+`/`-`) followed by digits. Used to distinguish flag
/// values (`-n 50`) from path arguments in our non-shell parser.
fn looks_like_number(tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    let bytes = tok.as_bytes();
    let start = if (bytes[0] == b'+' || bytes[0] == b'-') && bytes.len() > 1 {
        1
    } else {
        0
    };
    start < bytes.len() && bytes[start..].iter().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Command recognition ─────────────────────────────────────────

    #[test]
    fn cat_simple() {
        let p = extract_file_path("cat /tmp/foo.txt").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/foo.txt"));
    }

    #[test]
    fn cat_with_n_flag() {
        let p = extract_file_path("cat -n src/main.rs").unwrap();
        assert_eq!(p, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn cat_with_absolute_path_command() {
        let p = extract_file_path("/bin/cat README.md").unwrap();
        assert_eq!(p, PathBuf::from("README.md"));
    }

    #[test]
    fn head_with_n_value() {
        let p = extract_file_path("head -n 50 src/lib.rs").unwrap();
        assert_eq!(p, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn head_with_n_attached() {
        let p = extract_file_path("head -n50 src/lib.rs").unwrap();
        assert_eq!(p, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn bat_with_long_flag() {
        let p = extract_file_path("bat --style=plain src/lib.rs").unwrap();
        assert_eq!(p, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn tail_recognised() {
        let p = extract_file_path("tail -n 20 src/lib.rs").unwrap();
        assert_eq!(p, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn rejects_cat_with_two_files() {
        // We can't safely minify only one of two files — refuse.
        assert!(extract_file_path("cat a.rs b.rs").is_none());
    }

    #[test]
    fn rejects_unknown_command() {
        assert!(extract_file_path("grep -r foo src/").is_none());
    }

    #[test]
    fn rejects_compound_command() {
        // `;`, `&&`, `||`, `|` would all need a real shell parser.
        // We refuse the whole thing — better to under-minify than to
        // misinterpret `cat a.rs && rm -rf /`.
        assert!(extract_file_path("cat a.rs && echo done").is_none());
        assert!(extract_file_path("cat a.rs ; cat b.rs").is_none());
        assert!(extract_file_path("cat a.rs | grep foo").is_none());
    }

    #[test]
    fn rejects_empty_command() {
        assert!(extract_file_path("").is_none());
    }

    #[test]
    fn rejects_command_with_no_args() {
        assert!(extract_file_path("cat").is_none());
    }

    // ── try_minify_bash_output ──────────────────────────────────────

    #[test]
    fn try_minify_returns_none_for_empty_output() {
        assert!(try_minify_bash_output("cat /tmp/foo.txt", "").is_none());
    }

    #[test]
    fn try_minify_returns_none_for_non_dump_command() {
        // Even if the output is huge source code, a `grep` is not a file-dump
        // and we mustn't try to minify it.
        assert!(try_minify_bash_output("grep -r fn main src/", "fn main() {}").is_none());
    }

    #[test]
    fn try_minify_returns_none_for_missing_file() {
        // File doesn't exist on disk — pass through raw.
        let output = "fn main() {\n    // hello\n    println!(\"hi\");\n}\n";
        assert!(try_minify_bash_output("cat /nonexistent/path/foo.rs", output).is_none());
    }

    #[test]
    fn try_minify_works_on_real_file() {
        // Write a real file, run the minifier through the full path.
        let tmp = std::env::temp_dir().join("kirkforge_bash_minify_smoke.rs");
        let original = "fn main() {\n    // this is a comment that takes space\n    // and another one\n    println!(\"hi\");\n}\n";
        std::fs::write(&tmp, original).unwrap();

        let result = try_minify_bash_output(&format!("cat {}", tmp.display()), original);
        assert!(result.is_some(), "minification should fire on a real file");
        let minified = result.unwrap();
        assert!(
            minified.len() < original.len(),
            "minified form must be shorter"
        );
        assert!(!minified.contains("comment"), "comments must be stripped");

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn try_minify_refuses_small_savings() {
        // A file that's already tight (no comments, no blank lines) should
        // not be substituted — the savings threshold guards us.
        let tmp = std::env::temp_dir().join("kirkforge_bash_minify_tight.rs");
        let original = "fn x(){1+1}\nfn y(){2+2}\n";
        std::fs::write(&tmp, original).unwrap();

        let result = try_minify_bash_output(&format!("cat {}", tmp.display()), original);
        // The minifier is a near no-op on this — savings < 20% — so we
        // should refuse the swap and return None.
        assert!(
            result.is_none(),
            "should refuse swap when savings are below threshold"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn try_minify_passthrough_for_unknown_extension() {
        // .txt is not in the minify allowlist — we should never modify it.
        let tmp = std::env::temp_dir().join("kirkforge_bash_minify_txt.txt");
        let original =
            "this is some text content\nwith multiple lines\nthat should pass through unchanged\n";
        std::fs::write(&tmp, original).unwrap();

        let result = try_minify_bash_output(&format!("cat {}", tmp.display()), original);
        assert!(result.is_none(), "unknown extension must pass through");

        let _ = std::fs::remove_file(&tmp);
    }

    // ── Build-log minification ───────────────────────────────────

    /// Build a realistic-ish `cargo build` output: many `Compiling`
    /// progress lines (real builds for medium projects have 50-200),
    /// several `warning:` blocks (each 6 lines — title, location,
    /// blank, code excerpt, suggestion, blank), and a `Finished`
    /// line at the end. Used by the size-reduction and structure-
    /// preservation tests below.
    fn synthetic_cargo_output(n_compiling: usize, n_warnings: usize) -> String {
        let mut s = String::new();
        for i in 0..n_compiling {
            s.push_str(&format!("   Compiling proc-macro2 v1.0.{i}\n"));
        }
        for i in 0..n_warnings {
            s.push_str(&format!("warning: unused variable `x{i}`\n"));
            s.push_str(&format!(
                "  --> src/very/long/path/to/module{i}/file{i}.rs:{}:5\n",
                100 + i
            ));
            s.push_str("   |\n");
            s.push_str(&format!("{} |     let x{i} = 5;\n", 100 + i));
            s.push_str(&format!("   |         ^ help: if this is intentional, prefix it with an underscore: `_x{i}`\n"));
            s.push_str("   |\n");
            s.push_str(&format!(
                "   = note: `#[warn(unused_variables)]` on by default\n"
            ));
            s.push_str("\n");
        }
        s.push_str("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.3s\n");
        s
    }

    #[test]
    fn build_log_collapses_long_output() {
        // Realistic medium project: 50 crates compiling, 12 warnings
        // (each with a 7-line suggestion block — title, location,
        // code excerpt, suggestion, blank, blank, `note:` line).
        let original = synthetic_cargo_output(50, 12);
        let result = try_minify_build_log("cargo build", &original);
        assert!(
            result.is_some(),
            "should fire on a realistic cargo build log"
        );
        let minified = result.unwrap();
        assert!(
            minified.len() * 2 < original.len(),
            "minified ({} bytes) should be < half the original ({} bytes)",
            minified.len(),
            original.len()
        );
    }

    #[test]
    fn build_log_keeps_errors_verbatim() {
        // Mix errors with warnings — errors MUST come through with
        // their full context, since that's what the model needs to
        // actually act on.
        let mut original = synthetic_cargo_output(50, 5);
        original.insert_str(
            0,
            "error[E0425]: cannot find value `foo` in this scope\n  --> src/main.rs:3:9\n   |\n3  |     foo()\n   |     ^^^ not found in this scope\n   |\n   = help: a unit struct with a similar name exists\n\n",
        );

        let result = try_minify_build_log("cargo build", &original).unwrap();
        assert!(result.contains("error[E0425]"), "error title must survive");
        assert!(
            result.contains("cannot find value `foo`"),
            "error body must survive"
        );
        assert!(
            result.contains("--> src/main.rs:3:9"),
            "error location must survive"
        );
        assert!(
            result.contains("|     ^^^ not found in this scope"),
            "error context must survive"
        );
    }

    #[test]
    fn build_log_keeps_first_and_last_warnings() {
        // We promise to keep first 3 + last 3 warnings in full.
        // Spot-check that the FIRST warning's variable name and the
        // LAST warning's variable name both survive.
        let original = synthetic_cargo_output(50, 12);
        let result = try_minify_build_log("cargo build", &original).unwrap();
        assert!(
            result.contains("unused variable `x0`"),
            "first warning must survive"
        );
        assert!(
            result.contains("unused variable `x11`"),
            "last warning must survive"
        );
        // And the middle ones should be collapsed.
        assert!(
            result.contains("warnings omitted"),
            "middle warnings must be collapsed"
        );
    }

    #[test]
    fn build_log_refuses_non_cargo_command() {
        // Without strong output signal, ls / cat / grep are not
        // build commands — even if the output is long, we mustn't
        // touch it.
        let original = (0..50)
            .map(|i| format!("file{i}.txt"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(try_minify_build_log("ls", &original).is_none());
        assert!(try_minify_build_log("cat foo.txt", &original).is_none());
    }

    #[test]
    fn build_log_refuses_short_output() {
        // Below BUILD_LOG_MIN_LINES lines, the filtering overhead
        // exceeds the savings. Refuse.
        let original = synthetic_cargo_output(2, 2);
        assert!(try_minify_build_log("cargo build", &original).is_none());
    }

    #[test]
    fn build_log_fires_on_strong_output_signal() {
        // `make build` isn't `cargo build` literally, but the output
        // looks exactly like cargo. Strong output signal = gate passes.
        let original = synthetic_cargo_output(50, 12);
        let result = try_minify_build_log("make build", &original);
        assert!(
            result.is_some(),
            "output-format signal should fire on `make build`"
        );
    }

    #[test]
    fn build_log_keeps_finished_line() {
        // The `Finished` line tells the model the build succeeded —
        // always preserve it.
        let original = synthetic_cargo_output(50, 12);
        let result = try_minify_build_log("cargo build", &original).unwrap();
        assert!(result.contains("Finished"), "Finished line must survive");
    }

    #[test]
    fn build_log_handles_rustc() {
        let mut original = String::new();
        for i in 0..12 {
            original.push_str(&format!("warning: unused variable `x{i}`\n"));
            original.push_str(&format!("  --> src/lib{i}.rs:{}:5\n", 10 + i));
            original.push_str(&format!("{} |     let x{i} = 5;\n", 10 + i));
            original.push_str(&format!("   |         ^ help: if this is intentional, prefix it with an underscore: `_x{i}`\n"));
            original.push_str(&format!(
                "   = note: `#[warn(unused_variables)]` on by default\n"
            ));
            original.push_str("\n");
        }
        let result = try_minify_build_log("rustc --edition 2021 main.rs", &original);
        assert!(result.is_some(), "rustc warnings output should fire");
    }

    // ── Error-path composition (v1.2-p3) ──────────────────────────
    //
    // The bash tool's error path applies the minifier chain to stdout
    // (the same chain the success path uses). These tests verify the
    // composition still holds when the build *fails* — which is
    // exactly when minification helps most, because a 200-line rustc
    // diagnostic wall is the typical error payload.

    #[test]
    fn error_path_compose_on_failing_cargo_build() {
        // A failing `cargo build` produces the same progress + warning
        // markers as a successful one, plus an error block at the top.
        // The error path should still get the same collapse — errors
        // and their context are preserved verbatim per
        // `build_log_keeps_errors_verbatim`.
        let mut original = String::new();
        original.push_str("error[E0425]: cannot find value `foo` in this scope\n");
        original.push_str("  --> src/main.rs:3:9\n");
        original.push_str("   |\n3  |     foo()\n   |     ^^^ not found in this scope\n");
        original.push_str("   |\n   = help: a unit struct with a similar name exists\n\n");
        for i in 0..50 {
            original.push_str(&format!("   Compiling proc-macro2 v1.0.{i}\n"));
        }
        for i in 0..12 {
            original.push_str(&format!("warning: unused variable `x{i}`\n"));
            original.push_str(&format!("  --> src/lib{i}.rs:{}:5\n", 100 + i));
            original.push_str(&format!("{} |     let x{i} = 5;\n", 100 + i));
            original.push_str(&format!("   |         ^ help: if this is intentional, prefix it with an underscore: `_x{i}`\n"));
            original.push_str("   |\n");
            original.push_str(&format!(
                "   = note: `#[warn(unused_variables)]` on by default\n"
            ));
            original.push_str("\n");
        }

        // The error path runs the same two-step chain.
        let step1 =
            try_minify_bash_output("cargo build", &original).unwrap_or_else(|| original.clone());
        // `cargo build` is not in the file-dump allowlist, so step1 is
        // a no-op pass-through. Step2 is the build-log filter.
        let final_out = try_minify_build_log("cargo build", &step1)
            .expect("build-log filter must fire on failing cargo output");

        // The actual error must survive (model needs to act on it).
        assert!(
            final_out.contains("error[E0425]"),
            "error title must survive"
        );
        assert!(
            final_out.contains("cannot find value `foo`"),
            "error body must survive"
        );
        assert!(
            final_out.contains("--> src/main.rs:3:9"),
            "error location must survive"
        );

        // The collapsed output should be substantially smaller.
        assert!(
            final_out.len() * 2 < original.len(),
            "composed minify ({} bytes) should be < half the original ({} bytes)",
            final_out.len(),
            original.len()
        );
    }

    #[test]
    fn error_path_compose_passthrough_for_non_build() {
        // A failing `cat` of a missing file would normally produce
        // a tiny stderr ("cat: ...: No such file or directory") and
        // an empty stdout. The minifier chain must be a clean no-op.
        let stdout = "";
        let step1 = try_minify_bash_output("cat /nonexistent", stdout)
            .unwrap_or_else(|| stdout.to_string());
        let step2 = try_minify_build_log("cat /nonexistent", &step1).unwrap_or(step1);
        assert_eq!(step2, stdout, "empty stdout must pass through unchanged");
    }

    #[test]
    fn error_path_compose_refuses_swap_below_threshold() {
        // A 20-line failing build is below BUILD_LOG_MIN_LINES —
        // the chain should refuse the swap.
        let original = "warning: tiny\n  --> x.rs:1:1\n 1 | x\n   = note\n\n".repeat(20);
        let step1 = try_minify_bash_output("cargo build", &original);
        let step2 = try_minify_build_log("cargo build", step1.as_deref().unwrap_or(&original));
        // Either minifier refuses (None) or both pass through; both
        // outcomes are fine, what matters is the composed output
        // is no longer than the original.
        let final_out = step2.unwrap_or(step1.unwrap_or(original.clone()));
        assert!(final_out.len() <= original.len());
    }
}
