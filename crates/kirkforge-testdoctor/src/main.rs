//! kirkforge-testdoctor — test-performance doctor for Rust workspaces.
//!
//! Profiles the `cargo test` suite, classifies tests as fast/medium/slow,
//! partitions the suite into fast/full/coverage manifests, suggests
//! fixes for slow tests, analyzes coverage gaps, and self-diagnoses
//! untested code. See `docs/ideas/test-doctor.md` for the design.

mod classify;
mod diagnose;
mod flaky;
mod gaps;
mod partition;
mod profile;
mod suggest;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
    /// Capture per-test timings (nightly JSON if available, per-binary
    /// fallback otherwise). WO 12.5.
    ProfilePerTest,
    /// Read the profile and classify tests as fast/medium/slow/ignored.
    Classify,
    /// Generate fast-suite.json, full-suite.json, coverage-suite.json.
    Partition,
    /// Print fix suggestions for slow tests.
    Suggest,
    /// Analyze coverage gaps from a Cobertura XML file.
    Gaps {
        /// Path to the tarpaulin Cobertura XML.
        #[arg(long)]
        xml: PathBuf,
    },
    /// Self-diagnose: scan source files for untested public functions.
    Diagnose {
        /// Project root to scan (default: current directory).
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Detect a flaky test by running it N times (WO 12.5). Slow dev tool.
    Flaky {
        /// Test filter (passed to `cargo test -- <filter> --exact`).
        filter: String,
        /// Number of runs. Default 10.
        #[arg(long, default_value_t = 10)]
        runs: u32,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Profile => profile::run(&cli.profile),
        Cmd::ProfilePerTest => {
            let per = profile::profile_per_test(Some(std::path::Path::new(&cli.profile)))?;
            let class = classify::classify_per_test(&per);
            println!(
                "{:<40} {:<8} {:>10} {:<6}",
                "test", "speed", "dur_ms", "pass"
            );
            println!("{}", "-".repeat(70));
            for t in &class.tests {
                println!(
                    "{:<40} {:<8} {:>10} {:<6}",
                    t.profile.name,
                    t.speed.as_str(),
                    t.profile.duration_ms,
                    if t.profile.ignored {
                        "ign"
                    } else if t.profile.passed {
                        "ok"
                    } else {
                        "FAIL"
                    }
                );
            }
            println!("{}", "-".repeat(70));
            println!(
                "summary: fast={} ({}ms)  medium={} ({}ms)  slow={} ({}ms)  ignored={}{}",
                class.summary.fast,
                class.summary.fast_total_ms,
                class.summary.medium,
                class.summary.medium_total_ms,
                class.summary.slow,
                class.summary.slow_total_ms,
                class.summary.ignored,
                if class.coarse {
                    "  (coarse — stable fallback)"
                } else {
                    ""
                },
            );
            println!();
            suggest::run_per_test(&per)?;
            Ok(())
        }
        Cmd::Classify => classify::run(&cli.profile),
        Cmd::Partition => partition::run(&cli.profile, &cli.out),
        Cmd::Suggest => suggest::run(&cli.profile),
        Cmd::Gaps { xml } => {
            let gaps = gaps::analyze_gaps(&xml)?;
            gaps::print_report(&gaps);
            Ok(())
        }
        Cmd::Diagnose { root } => {
            let report = diagnose::diagnose(&root)?;
            diagnose::print_report(&report);
            Ok(())
        }
        Cmd::Flaky { runs, filter } => {
            let report = flaky::detect_flaky(&filter, runs)?;
            flaky::print_report(&report);
            Ok(())
        }
    }
}
