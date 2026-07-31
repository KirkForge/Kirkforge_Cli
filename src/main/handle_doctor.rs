// `kirkforge doctor <subcommand>` dispatch (WO 12.4, ADR-0029).
// Extracted from the binary root — pure move, no behaviour change.

use kirkforge::cli::DoctorCommand;

/// Handle `kirkforge doctor <subcommand>` (WO 12.4, ADR-0029).
/// Dispatches to the kirkforge-testdoctor library.
pub(super) fn handle_doctor_command(command: DoctorCommand) -> anyhow::Result<()> {
    use kirkforge_testdoctor as td;
    match command {
        DoctorCommand::Profile => td::profile::run("test-profile.json"),
        DoctorCommand::ProfilePerTest => {
            let per = td::profile::profile_per_test(Some(std::path::Path::new(
                "test-profile-per-test.json",
            )))?;
            let class = td::classify::classify_per_test(&per);
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
            td::suggest::run_per_test(&per)?;
            Ok(())
        }
        DoctorCommand::Classify => td::classify::run("test-profile.json"),
        DoctorCommand::Partition => td::partition::run("test-profile.json", "test-suites"),
        DoctorCommand::Suggest => td::suggest::run("test-profile.json"),
        DoctorCommand::SuggestDetailed { filter } => {
            let per = td::profile::profile_per_test(Some(std::path::Path::new(
                "test-profile-per-test.json",
            )))?;
            td::suggest::run_suggest_detailed(&per, filter.as_deref())?;
            Ok(())
        }
        DoctorCommand::Apply {
            suggestion,
            test,
            yes,
        } => {
            let kind = parse_doctor_kind_from_id(&suggestion)
                .ok_or_else(|| anyhow::anyhow!("could not parse suggestion id `{suggestion}`"))?;
            let test_name = suggestion
                .split("::")
                .next()
                .unwrap_or(&suggestion)
                .to_string();
            let s = td::suggest::Suggestion {
                id: suggestion.clone(),
                test: test_name,
                severity: "medium".to_string(),
                fix: String::new(),
                rationale: String::new(),
                kind,
            };
            let diff = td::apply::apply_suggestion(std::path::Path::new(&test), &s, yes)?;
            println!("{diff}");
            if !yes {
                println!("\n(dry-run — pass --yes to write)");
            }
            Ok(())
        }
        DoctorCommand::Gaps { xml } => {
            let gaps = td::gaps::analyze_gaps(&xml)?;
            td::gaps::print_report(&gaps);
            Ok(())
        }
        DoctorCommand::Diagnose { root } => {
            let report = td::diagnose::diagnose(&root)?;
            td::diagnose::print_report(&report);
            Ok(())
        }
        DoctorCommand::Flaky { runs, filter } => {
            let report = td::flaky::detect_flaky(&filter, runs)?;
            td::flaky::print_report(&report);
            Ok(())
        }
    }
}

/// Parse the `SuggestionKind` slug out of a suggestion id of the form
/// `<test>::<kind_slug>`. Returns `None` if the slug is unrecognized.
fn parse_doctor_kind_from_id(id: &str) -> Option<kirkforge_testdoctor::suggest::SuggestionKind> {
    use kirkforge_testdoctor::suggest::SuggestionKind;
    let slug = id.split("::").nth(1)?;
    Some(match slug {
        "ignore_slow" => SuggestionKind::IgnoreSlow,
        "tokio_start_paused" => SuggestionKind::TokioStartPaused,
        "env_guard" => SuggestionKind::EnvGuard,
        "mock_subprocess" => SuggestionKind::MockSubprocess,
        "wiremock" => SuggestionKind::Wiremock,
        "named_temp_file" => SuggestionKind::NamedTempFile,
        _ => return None,
    })
}
