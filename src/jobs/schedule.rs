//! Scheduled-job data model and schedule parsing.
//!
//! Supports standard 5-field cron (`* * * * *`), convenience aliases
//! (`@hourly`, `@daily`, `@weekly`, `@restart`, `@once <ISO-8601>`), and
//! one-shot scheduling. The `cron` crate computes upcoming run times; aliases
//! are expanded into normalised cron strings before parsing.

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// A persisted scheduled job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduledJob {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub schedule: ScheduleSpec,
    pub kind: JobKind,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<JobRunSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<DateTime<Utc>>,
}

fn default_enabled() -> bool {
    true
}

/// How a job recurs (or runs once).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value")]
pub enum ScheduleSpec {
    /// Normalised cron expression. The stored string always has a seconds
    /// field so the `cron` crate can parse it directly; the UI may display it
    /// without the leading `0 ` for familiarity.
    #[serde(rename = "cron")]
    Cron(String),
    /// Run once at the given UTC time and then become disabled.
    #[serde(rename = "once")]
    Once(DateTime<Utc>),
    /// Run once when the scheduler daemon starts, then disable.
    #[serde(rename = "restart")]
    Restart,
}

/// What the job should do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value")]
pub enum JobKind {
    /// Execute a shell command through the same safety gate as the model's
    /// `bash` tool.
    #[serde(rename = "bash")]
    Bash { command: String },
    /// Reserved for future work: invoke a built-in or plugin skill.
    #[serde(rename = "skill")]
    Skill { name: String, args: Vec<String> },
}

/// Result of a single scheduled-job execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobRunSummary {
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: RunStatus,
    pub exit_code: Option<i32>,
    pub stdout_path: std::path::PathBuf,
    pub stderr_path: std::path::PathBuf,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum RunStatus {
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "failure")]
    Failure,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl RunStatus {
    /// Short human-readable icon + word used in TUI notifications.
    pub fn label(self) -> &'static str {
        match self {
            RunStatus::Success => "success",
            RunStatus::Failure => "failure",
            RunStatus::Cancelled => "cancelled",
        }
    }
}

/// Generate a stable, human-readable job id based on the current UTC date and
/// the next available sequence number in the jobs directory.
pub fn generate_job_id(jobs_dir: &Path) -> Result<String> {
    let date = Utc::now().format("%Y%m%d").to_string();
    let mut max_seq: u32 = 0;
    if jobs_dir.is_dir() {
        for entry in std::fs::read_dir(jobs_dir)
            .with_context(|| format!("reading jobs directory {}", jobs_dir.display()))?
            .flatten()
        {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let prefix = format!("job-{date}-");
            if let Some(rest) = name.strip_prefix(&prefix) {
                if let Ok(seq) = rest.parse::<u32>() {
                    max_seq = max_seq.max(seq);
                }
            }
        }
    }
    Ok(format!("job-{date}-{:03}", max_seq + 1))
}

/// Parse a user-supplied schedule expression.
///
/// Supported forms:
/// - `@hourly`, `@daily`, `@weekly`
/// - `@once <ISO-8601>` (e.g. `2026-07-20T09:00:00` or RFC3339 with timezone)
/// - `@restart`
/// - raw 5-field or 6-field cron
pub fn parse_schedule(input: &str) -> Result<ScheduleSpec> {
    let trimmed = input.trim();

    if trimmed.eq_ignore_ascii_case("@hourly") {
        return Ok(ScheduleSpec::Cron("0 0 * * * *".into()));
    }
    if trimmed.eq_ignore_ascii_case("@daily") {
        return Ok(ScheduleSpec::Cron("0 0 0 * * *".into()));
    }
    if trimmed.eq_ignore_ascii_case("@weekly") {
        return Ok(ScheduleSpec::Cron("0 0 0 * * 0".into()));
    }
    if trimmed.eq_ignore_ascii_case("@restart") {
        return Ok(ScheduleSpec::Restart);
    }

    if let Some(stripped) = trimmed
        .to_lowercase()
        .strip_prefix("@once ")
        .map(|_| trimmed.replacen("@once ", "", 1))
    {
        let when = parse_iso_datetime(&stripped)?;
        return Ok(ScheduleSpec::Once(when));
    }

    // Treat the rest as cron. Normalise 5-field (minute-level) expressions to
    // 6-field (seconds-level) so the cron crate can parse them.
    let normalised = normalise_cron_expression(trimmed);
    let _ = cron::Schedule::from_str(&normalised)
        .with_context(|| format!("invalid cron expression: {trimmed}"))?;
    Ok(ScheduleSpec::Cron(normalised))
}

/// Parse an ISO-8601-like datetime and convert it to UTC.
fn parse_iso_datetime(s: &str) -> Result<DateTime<Utc>> {
    let s = s.trim();

    // RFC3339 / ISO-8601 with explicit timezone.
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Plain `YYYY-MM-DDTHH:MM:SS` (no timezone) — interpret as UTC.
    let formats = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M"];
    for fmt in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
        }
    }

    // Date only — default to midnight UTC.
    if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(naive) = date.and_hms_opt(0, 0, 0) {
            return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
        }
    }

    anyhow::bail!("could not parse scheduled time '{s}' as ISO-8601")
}

/// If the expression has five whitespace-separated fields, prepend `0 ` so it
/// becomes seconds-minutes-hours-day_of_month-month-day_of_week. Six-field
/// expressions are left untouched.
fn normalise_cron_expression(expr: &str) -> String {
    let field_count = expr.split_whitespace().count();
    if field_count == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

/// Compute the next run time at or after `after` for a given schedule.
pub fn compute_next_run(schedule: &ScheduleSpec, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match schedule {
        ScheduleSpec::Once(t) => {
            if *t >= after {
                Some(*t)
            } else {
                None
            }
        }
        ScheduleSpec::Restart => Some(after),
        ScheduleSpec::Cron(expr) => {
            let schedule = cron::Schedule::from_str(expr).ok()?;
            schedule.after(&after).next()
        }
    }
}

/// Strip the synthetic seconds field from a stored cron expression so it can
/// be displayed in the familiar 5-field form when it was originally entered as
/// 5-field. If the seconds field is anything other than `0`, keep the 6-field
/// form to avoid losing information.
pub fn display_cron(expr: &str) -> String {
    let mut parts = expr.split_whitespace();
    let first = parts.next();
    let rest: Vec<&str> = parts.collect();
    if rest.len() == 5 && first == Some("0") {
        rest.join(" ")
    } else {
        expr.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Timelike};

    #[test]
    fn parse_aliases() {
        assert!(matches!(
            parse_schedule("@hourly").unwrap(),
            ScheduleSpec::Cron(_)
        ));
        assert!(matches!(
            parse_schedule("@daily").unwrap(),
            ScheduleSpec::Cron(_)
        ));
        assert!(matches!(
            parse_schedule("@weekly").unwrap(),
            ScheduleSpec::Cron(_)
        ));
        assert!(matches!(
            parse_schedule("@restart").unwrap(),
            ScheduleSpec::Restart
        ));
    }

    #[test]
    fn parse_once_iso8601() {
        let dt = parse_schedule("@once 2026-07-20T09:00:00").unwrap();
        assert!(matches!(dt, ScheduleSpec::Once(_)));
        if let ScheduleSpec::Once(t) = dt {
            assert_eq!(t.to_rfc3339(), "2026-07-20T09:00:00+00:00");
        }
    }

    #[test]
    fn parse_once_date_only_is_utc_midnight() {
        let dt = parse_schedule("@once 2026-07-20").unwrap();
        if let ScheduleSpec::Once(t) = dt {
            assert_eq!(t.to_rfc3339(), "2026-07-20T00:00:00+00:00");
        } else {
            panic!("expected Once");
        }
    }

    #[test]
    fn parse_five_field_cron() {
        let s = parse_schedule("0 9 * * 1-5").unwrap();
        if let ScheduleSpec::Cron(expr) = s {
            assert_eq!(expr, "0 0 9 * * 1-5");
        } else {
            panic!("expected Cron");
        }
    }

    #[test]
    fn parse_six_field_cron_left_alone() {
        let s = parse_schedule("30 0 9 * * 1-5").unwrap();
        if let ScheduleSpec::Cron(expr) = s {
            assert_eq!(expr, "30 0 9 * * 1-5");
        } else {
            panic!("expected Cron");
        }
    }

    #[test]
    fn invalid_cron_rejected() {
        assert!(parse_schedule("not-a-cron").is_err());
        assert!(parse_schedule("1 2 3").is_err());
    }

    #[test]
    fn past_once_job_has_no_next_run() {
        let past = Utc::now() - Duration::hours(1);
        let spec = ScheduleSpec::Once(past);
        assert!(compute_next_run(&spec, Utc::now()).is_none());
    }

    #[test]
    fn future_once_job_next_run_is_exact_time() {
        let future = Utc::now() + Duration::hours(1);
        let future = future.with_nanosecond(0).unwrap();
        let spec = ScheduleSpec::Once(future);
        assert_eq!(compute_next_run(&spec, Utc::now()).unwrap(), future);
    }

    #[test]
    fn daily_cron_next_run_is_tomorrow_midnight() {
        let now = Utc::now();
        let spec = parse_schedule("@daily").unwrap();
        let next = compute_next_run(&spec, now).unwrap();
        let tomorrow = (now + Duration::days(1)).date_naive();
        assert_eq!(next.date_naive(), tomorrow);
        assert_eq!(next.hour(), 0);
        assert_eq!(next.minute(), 0);
        assert_eq!(next.second(), 0);
    }

    #[test]
    fn restart_next_run_is_now() {
        let now = Utc::now();
        let next = compute_next_run(&ScheduleSpec::Restart, now).unwrap();
        // Restart means run immediately; allow a few seconds of clock drift.
        assert!((next - now).num_seconds().abs() < 2);
    }

    #[test]
    fn display_cron_strips_zero_seconds() {
        assert_eq!(display_cron("0 0 9 * * 1-5"), "0 9 * * 1-5");
        assert_eq!(display_cron("30 0 9 * * 1-5"), "30 0 9 * * 1-5");
    }

    #[test]
    fn generate_job_id_increments_sequence() {
        let tmp = tempfile::tempdir().unwrap();
        let jobs_dir = tmp.path().join("jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        let date = Utc::now().format("%Y%m%d").to_string();
        std::fs::create_dir_all(jobs_dir.join(format!("job-{date}-001"))).unwrap();
        std::fs::create_dir_all(jobs_dir.join(format!("job-{date}-003"))).unwrap();
        assert_eq!(
            generate_job_id(&jobs_dir).unwrap(),
            format!("job-{date}-004")
        );
    }
}
