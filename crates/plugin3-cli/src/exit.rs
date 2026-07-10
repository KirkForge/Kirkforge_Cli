//! Exit-code helpers per ADR-0015 § Exit codes.
//!
//! ponytail: two helpers, one per non-default code. The default
//! success path uses `?` and falls through; callers only reach
//! here when they want a specific exit code that `Result` cannot
//! express. Adding a new exit code is one `pub fn` line — no
//! dispatch table, no builder, no trait.

/// Config parse or backend init failure → `EX_CONFIG` (BSD sysexits).
pub fn exit_config_err(msg: &str) -> ! {
    eprintln!("plugin3: {msg}");
    std::process::exit(78);
}

/// User-visible usage error (distinct from `clap`'s built-in 64,
/// which fires only on flag parsing).
pub fn exit_usage_err(msg: &str) -> ! {
    eprintln!("plugin3: {msg}");
    std::process::exit(64);
}

// ponytail: a parallel `[cli] exit-codes` drift test (in main.rs)
// shells out to `plugin3 config --validate` against a tempdir with
// an unwritable parent and asserts the exit code is 78. That test
// pins both this helper *and* the validate path through it in one
// subprocess invocation — no need for a separate per-helper spawn
// here (the helper is two lines of `eprintln!` + `exit`).
