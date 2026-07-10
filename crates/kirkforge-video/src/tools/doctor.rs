//! `kf doctor` — probe ffmpeg capabilities and report what's missing.
//!
//! Render failures often come down to a missing encoder or filter
//! (libx264 not built in, xfade dropped in a stripped build, etc.).
//! This module shells out to ffmpeg, parses its self-reports, and
//! emits PASS/FAIL for the capabilities KirkForge-Video actually
//! needs.
//!
//! ponytail: pure parsing helpers are unit-tested against canned
//! ffmpeg output. The shell-call wrapper is left untested because
//! ffmpeg's actual output varies across builds and CI wouldn't have
//! a fixed fixture.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub ffmpeg_path: String,
    pub version: Option<String>,
    pub checks: Vec<Check>,
}

impl DoctorReport {
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }
}

/// ponytail: shell out to ffmpeg and run a single subcommand. Returns
/// stdout as a String (lossy). On failure (binary missing, nonzero
/// exit) returns None — the caller decides whether that's a fatal
/// error or a single failed check.
fn ffmpeg_capture(ffmpeg: &str, subargs: &[&str]) -> Option<String> {
    let out = Command::new(ffmpeg).args(subargs).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn run_doctor(ffmpeg_path: &str) -> DoctorReport {
    let version_raw = ffmpeg_capture(ffmpeg_path, &["-version"]);
    let encoders_raw = ffmpeg_capture(ffmpeg_path, &["-encoders"]);
    let filters_raw = ffmpeg_capture(ffmpeg_path, &["-filters"]);

    let version = version_raw
        .as_deref()
        .and_then(parse_version_line)
        .map(String::from);

    let encoders: Vec<String> = encoders_raw
        .as_deref()
        .map(parse_name_list)
        .unwrap_or_default();
    let filters: Vec<String> = filters_raw
        .as_deref()
        .map(parse_name_list)
        .unwrap_or_default();

    let mut checks = Vec::new();

    checks.push(Check {
        name: "ffmpeg available".into(),
        passed: version_raw.is_some(),
        detail: version
            .clone()
            .unwrap_or_else(|| format!("{ffmpeg_path} not found or returned non-zero")),
    });

    if let Some(v) = &version {
        checks.push(Check {
            name: "ffmpeg version reported".into(),
            passed: true,
            detail: v.clone(),
        });
    }

    let want_encoders = ["libx264", "aac"];
    for enc in want_encoders {
        checks.push(Check {
            name: format!("encoder {enc} present"),
            passed: encoders.iter().any(|e| e == enc),
            detail: if encoders.iter().any(|e| e == enc) {
                format!("{enc} found")
            } else {
                format!("{enc} missing — KirkForge render needs it")
            },
        });
    }

    let want_filters = ["xfade", "drawtext", "drawbox", "subtitles"];
    for f in want_filters {
        checks.push(Check {
            name: format!("filter {f} present"),
            passed: filters.iter().any(|x| x == f),
            detail: if filters.iter().any(|x| x == f) {
                format!("{f} found")
            } else {
                format!("{f} missing — some scene types may not render")
            },
        });
    }

    DoctorReport {
        ffmpeg_path: ffmpeg_path.to_string(),
        version,
        checks,
    }
}

/// Parse the first line of `ffmpeg -version`. Returns None if the
/// line doesn't look like an ffmpeg banner.
pub fn parse_version_line(raw: &str) -> Option<&str> {
    let first = raw.lines().next()?;
    // Strip a possible "ffmpeg version N-..." or "ffmpeg N-..." prefix
    // down to a human-readable string. ponytail: keep the full first
    // line — it has the build config and copyright too.
    Some(first.trim())
}

/// Parse ffmpeg's `-encoders` or `-filters` output into a list of
/// short names.
///
/// ponytail: ffmpeg's column widths differ between subcommands
/// (encoders: 7-char flag prefix; filters: 5-char flag prefix). Don't
/// hardcode a column — instead take the FIRST whitespace-delimited
/// token and skip the second if it looks like a header marker
/// (starts with `=`, or is one of the known type words "Video",
/// "Audio", "Subtitle"). Real ffmpeg always lays out lines as
/// `<flag-marker> <short_name> <long_description>`, so this works
/// for both commands.
pub fn parse_name_list(raw: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('-') {
            continue;
        }
        // Header lines like "Encoders:" or "Filters:" — single word,
        // ends with ':'. Skip.
        if trimmed.ends_with(':') && !trimmed.contains(' ') {
            continue;
        }
        // Walk tokens. Token 0 is the flag marker (e.g. "V.....",
        // "T.C", "..."). Token 1 is the name (or "=" for the type
        // header row "V..... = Video").
        let mut iter = trimmed.split_whitespace();
        let _flag = iter.next();
        let Some(name) = iter.next() else {
            continue;
        };
        if name == "=" {
            continue;
        }
        // Known type-header words that follow "=" on the divider line.
        if matches!(
            name,
            "Video" | "Audio" | "Subtitle" | "Filters" | "Encoders"
        ) {
            continue;
        }
        // Names that start with '(' are continuation lines.
        if name.starts_with('(') {
            continue;
        }
        names.push(name.to_string());
    }
    names
}

pub fn render_text_report(r: &DoctorReport) -> String {
    let mut s = String::new();
    s.push_str(&format!("ffmpeg: {}\n", r.ffmpeg_path));
    if let Some(v) = &r.version {
        s.push_str(&format!("version: {v}\n"));
    } else {
        s.push_str("version: <unavailable>\n");
    }
    s.push('\n');
    for c in &r.checks {
        let mark = if c.passed { "PASS" } else { "FAIL" };
        s.push_str(&format!("[{mark}] {} — {}\n", c.name, c.detail));
    }
    s
}

/// `kf doctor project` — validate a project's files end-to-end.
///
/// ponytail: pure file-existence + JSON-parsing checks. No shelling
/// out to ffmpeg; the rendered video's size-on-disk is enough of a
/// "did it actually render" signal here. The pipeline ran the ffmpeg
/// doctor already.
pub fn run_project_doctor(project_dir: &Path) -> DoctorReport {
    let mut checks = Vec::new();

    // Project dir exists and is a directory.
    let dir_ok = project_dir.is_dir();
    checks.push(Check {
        name: "project directory".into(),
        passed: dir_ok,
        detail: if dir_ok {
            format!("{}", project_dir.display())
        } else {
            format!("{}: not a directory", project_dir.display())
        },
    });

    // brief.txt — required, must be non-empty.
    let brief = project_dir.join("brief.txt");
    let brief_ok = brief.is_file()
        && std::fs::metadata(&brief)
            .map(|m| m.len() > 0)
            .unwrap_or(false);
    checks.push(Check {
        name: "brief.txt present".into(),
        passed: brief_ok,
        detail: if brief_ok {
            format!(
                "{} ({} bytes)",
                brief.display(),
                std::fs::metadata(&brief).map(|m| m.len()).unwrap_or(0)
            )
        } else {
            format!("{} missing or empty", brief.display())
        },
    });

    // brand.json — optional; missing is OK, malformed is FAIL.
    let brand = project_dir.join("brand.json");
    let brand_state = if !brand.exists() {
        Check {
            name: "brand.json".into(),
            passed: true,
            detail: "absent (using defaults)".into(),
        }
    } else {
        match std::fs::read_to_string(&brand)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        {
            Some(_) => Check {
                name: "brand.json".into(),
                passed: true,
                detail: format!("{} parses", brand.display()),
            },
            None => Check {
                name: "brand.json".into(),
                passed: false,
                detail: format!("{} present but invalid JSON", brand.display()),
            },
        }
    };
    checks.push(brand_state);

    // scene_plan.json — required after a pipeline run, must parse.
    let plan = project_dir.join("artifacts").join("scene_plan.json");
    let plan_ok = plan.is_file()
        && std::fs::read_to_string(&plan)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .is_some();
    checks.push(Check {
        name: "scene_plan.json".into(),
        passed: plan_ok,
        detail: if plan_ok {
            format!("{} parses", plan.display())
        } else {
            format!("{} missing or invalid", plan.display())
        },
    });

    // composition.json — required after a render, must parse.
    let comp = project_dir.join("artifacts").join("composition.json");
    let comp_ok = comp.is_file()
        && std::fs::read_to_string(&comp)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .is_some();
    checks.push(Check {
        name: "composition.json".into(),
        passed: comp_ok,
        detail: if comp_ok {
            format!("{} parses", comp.display())
        } else {
            format!("{} missing or invalid (run `kf render`)", comp.display())
        },
    });

    // risk_report.json — optional (some pipelines skip the gate).
    let risk = project_dir.join("artifacts").join("risk_report.json");
    let risk_ok = !risk.exists()
        || (risk.is_file()
            && std::fs::read_to_string(&risk)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .is_some());
    checks.push(Check {
        name: "risk_report.json".into(),
        passed: risk_ok,
        detail: if !risk.exists() {
            "absent (no risk gate run)".into()
        } else if risk_ok {
            format!("{} parses", risk.display())
        } else {
            format!("{} present but invalid", risk.display())
        },
    });

    // render/final.mp4 — required, must be >0 bytes.
    let out = project_dir.join("render").join("final.mp4");
    let out_size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    let out_ok = out.is_file() && out_size > 0;
    checks.push(Check {
        name: "render/final.mp4".into(),
        passed: out_ok,
        detail: if out_ok {
            format!("{} ({} bytes)", out.display(), out_size)
        } else {
            format!("{} missing or empty (run `kf render`)", out.display())
        },
    });

    DoctorReport {
        ffmpeg_path: String::new(),
        version: None,
        checks,
    }
}

pub fn write_report(dir: &Path, r: &DoctorReport) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join("doctor.json"),
        serde_json::to_string_pretty(r).unwrap_or_else(|_| "{}".into()),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_line_returns_first_nonempty_line() {
        let raw =
            "ffmpeg version 6.0 Copyright (c) 2000-2023 the FFmpeg developers\nbuilt with gcc\n";
        assert_eq!(
            parse_version_line(raw).unwrap(),
            "ffmpeg version 6.0 Copyright (c) 2000-2023 the FFmpeg developers"
        );
    }

    #[test]
    fn parse_version_line_returns_none_for_empty() {
        assert!(parse_version_line("").is_none());
    }

    #[test]
    fn parse_name_list_extracts_encoders() {
        // Faked ffmpeg -encoders output. Format: 7-char flag prefix
        // (1 type + 6 dot markers), space, name, space, long desc.
        let lines = [
            "Encoders:",
            " -------",
            " V..... = Video",
            " V..... libx264          libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10",
            " A..... aac              AAC (Advanced Audio Coding)",
            " S..... ass              ASS",
            " -------",
        ];
        let raw = lines.join("\n");
        let names = parse_name_list(&raw);
        assert!(names.contains(&"libx264".to_string()), "got: {names:?}");
        assert!(names.contains(&"aac".to_string()));
        assert!(names.contains(&"ass".to_string()));
    }

    #[test]
    fn parse_name_list_extracts_filters() {
        // Real ffmpeg -filters format: 5-char flag prefix (1 type +
        // 3 dots + 1 threading char), space, name, space, V->V, desc.
        let lines = [
            "Filters:",
            " -------",
            " T..... = Audio",
            " T.C drawbox             V->V       Draw a colored box",
            " ... drawtext            V->V       Draw text",
            " .S. xfade               VV->V      Cross fade",
            " ... subtitles           V->V       Render subtitles",
        ];
        let raw = lines.join("\n");
        let names = parse_name_list(&raw);
        assert_eq!(names, vec!["drawbox", "drawtext", "xfade", "subtitles"]);
    }

    #[test]
    fn doctor_report_all_passed_requires_no_failures() {
        let r = DoctorReport {
            ffmpeg_path: "ffmpeg".into(),
            version: Some("ffmpeg 6".into()),
            checks: vec![
                Check {
                    name: "a".into(),
                    passed: true,
                    detail: "ok".into(),
                },
                Check {
                    name: "b".into(),
                    passed: false,
                    detail: "missing".into(),
                },
            ],
        };
        assert!(!r.all_passed());
    }

    #[test]
    fn doctor_report_all_passed_when_all_true() {
        let r = DoctorReport {
            ffmpeg_path: "ffmpeg".into(),
            version: Some("ffmpeg 6".into()),
            checks: vec![Check {
                name: "a".into(),
                passed: true,
                detail: "ok".into(),
            }],
        };
        assert!(r.all_passed());
    }

    #[test]
    fn render_text_report_marks_passes_and_failures() {
        let r = DoctorReport {
            ffmpeg_path: "ffmpeg".into(),
            version: Some("ffmpeg 6.0".into()),
            checks: vec![
                Check {
                    name: "x".into(),
                    passed: true,
                    detail: "ok".into(),
                },
                Check {
                    name: "y".into(),
                    passed: false,
                    detail: "missing".into(),
                },
            ],
        };
        let s = render_text_report(&r);
        assert!(s.contains("[PASS] x"));
        assert!(s.contains("[FAIL] y"));
        assert!(s.contains("ffmpeg 6.0"));
    }

    fn make_project(dir: &Path, with_brand: bool, brand_valid: bool, with_render: bool) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("brief.txt"), "Hello\n- 50% stat\n").unwrap();
        let arts = dir.join("artifacts");
        std::fs::create_dir_all(&arts).unwrap();
        std::fs::write(
            arts.join("scene_plan.json"),
            r#"{"kind":"scene_plan","scenes":[]}"#,
        )
        .unwrap();
        std::fs::write(
            arts.join("composition.json"),
            r#"{"width":1920,"height":1080,"fps":30,"scenes":[]}"#,
        )
        .unwrap();
        if with_brand {
            if brand_valid {
                std::fs::write(dir.join("brand.json"), "{\"primary_color\":\"#ffcc00\"}").unwrap();
            } else {
                std::fs::write(dir.join("brand.json"), "{not json").unwrap();
            }
        }
        if with_render {
            let render = dir.join("render");
            std::fs::create_dir_all(&render).unwrap();
            std::fs::write(render.join("final.mp4"), b"fake-mp4-bytes").unwrap();
        }
    }

    #[test]
    fn project_doctor_passes_for_fully_built_project() {
        let tmp = std::env::temp_dir().join(format!("kf-doc-good-{}", std::process::id()));
        make_project(&tmp, true, true, true);
        let r = run_project_doctor(&tmp);
        assert!(
            r.all_passed(),
            "expected all checks pass, failed: {:?}",
            r.checks.iter().filter(|c| !c.passed).collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn project_doctor_passes_without_brand_or_render_because_brand_optional_and_render_required() {
        let tmp = std::env::temp_dir().join(format!("kf-doc-norender-{}", std::process::id()));
        make_project(&tmp, false, true, false);
        let r = run_project_doctor(&tmp);
        // No brand.json → PASS (optional). No render/final.mp4 → FAIL.
        assert!(!r.all_passed());
        let render_check = r
            .checks
            .iter()
            .find(|c| c.name == "render/final.mp4")
            .unwrap();
        assert!(!render_check.passed);
        let brand_check = r.checks.iter().find(|c| c.name == "brand.json").unwrap();
        assert!(
            brand_check.passed,
            "missing brand.json should pass as optional: {}",
            brand_check.detail
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn project_doctor_fails_on_malformed_brand_json() {
        let tmp = std::env::temp_dir().join(format!("kf-doc-badbrand-{}", std::process::id()));
        make_project(&tmp, true, false, true);
        let r = run_project_doctor(&tmp);
        let brand = r.checks.iter().find(|c| c.name == "brand.json").unwrap();
        assert!(
            !brand.passed,
            "malformed brand.json should fail: {}",
            brand.detail
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn project_doctor_fails_when_brief_missing() {
        let tmp = std::env::temp_dir().join(format!("kf-doc-nobrief-{}", std::process::id()));
        make_project(&tmp, false, true, true);
        std::fs::remove_file(tmp.join("brief.txt")).unwrap();
        let r = run_project_doctor(&tmp);
        let brief = r
            .checks
            .iter()
            .find(|c| c.name == "brief.txt present")
            .unwrap();
        assert!(!brief.passed);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn project_doctor_fails_when_render_mp4_is_empty() {
        let tmp = std::env::temp_dir().join(format!("kf-doc-empty-{}", std::process::id()));
        make_project(&tmp, false, true, true);
        std::fs::write(tmp.join("render").join("final.mp4"), b"").unwrap();
        let r = run_project_doctor(&tmp);
        let render = r
            .checks
            .iter()
            .find(|c| c.name == "render/final.mp4")
            .unwrap();
        assert!(!render.passed, "empty mp4 should fail: {}", render.detail);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn project_doctor_fails_on_missing_project_dir() {
        let tmp = std::env::temp_dir().join(format!("kf-doc-nodir-{}-nope", std::process::id()));
        let r = run_project_doctor(&tmp);
        let dir = r
            .checks
            .iter()
            .find(|c| c.name == "project directory")
            .unwrap();
        assert!(!dir.passed);
        assert!(!r.all_passed());
    }
}
