//! kirkforge-testdoctor — test-performance doctor for Rust workspaces.
//!
//! Profiles the `cargo test` suite, classifies tests as fast/medium/slow,
//! partitions the suite into fast/full/coverage manifests, and suggests
//! fixes for slow tests. See `docs/ideas/test-doctor.md` for the design.

mod classify;
mod partition;
mod profile;
mod suggest;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "kirkforge-testdoctor",
    version,
    about = "Test-performance doctor for Rust workspaces."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    /// Path to the profile JSON (default: ./test-profile.json).
    #[arg(long, global = true, default_value = "test-profile.json")]
    profile: String,

    /// Directory to write partition manifests (default: ./test-suites).
    #[arg(long, global = true, default_value = "test-suites")]
    out: String,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run `cargo test --workspace --no-fail-fast` and capture per-binary timings.
    Profile,
    /// Read the profile and classify tests as fast/medium/slow/ignored.
    Classify,
    /// Generate fast-suite.json, full-suite.json, coverage-suite.json.
    Partition,
    /// Print fix suggestions for slow tests.
    Suggest,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Profile => profile::run(&cli.profile),
        Cmd::Classify => classify::run(&cli.profile),
        Cmd::Partition => partition::run(&cli.profile, &cli.out),
        Cmd::Suggest => suggest::run(&cli.profile),
    }
}
