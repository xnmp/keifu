//! CI check details for an open PR, via the `gh` CLI.
//!
//! Two lazy, background fetches (never on the UI thread):
//! - the check list (`gh pr checks <n> --json …`) when the popup opens, and
//! - a failed check's log tail (`gh run view <run-id> --log-failed`) when the
//!   user selects it, cached per run for the session.
//!
//! Everything above `CheckFetch` is pure and unit-tested.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use chrono::DateTime;
use serde::Deserialize;

/// Timeout for the check-list query.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for a (potentially large) failure-log fetch.
const LOG_TIMEOUT: Duration = Duration::from_secs(25);
/// Keep only the last N lines of a failure log — errors live at the tail.
pub const LOG_TAIL_LINES: usize = 200;

/// Outcome of a single check, coarsened to what the UI shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Pass,
    Fail,
    Pending,
    Skipped,
}

impl CheckState {
    /// Glyph shown for this state in the checks popup (colored by the caller
    /// with the matching `pr_ci_*` theme color).
    pub fn icon(self) -> char {
        match self {
            Self::Pass => '✓',
            Self::Fail => '✗',
            Self::Pending => '●',
            Self::Skipped => '○',
        }
    }

    /// Map from gh's `bucket` (its own categorization) with a fallback to the
    /// raw `state` vocabulary (shared with statusCheckRollup).
    fn from_gh(state: &str, bucket: &str) -> Self {
        match bucket.to_ascii_lowercase().as_str() {
            "pass" => Self::Pass,
            "fail" | "cancel" => Self::Fail,
            "pending" => Self::Pending,
            "skipping" => Self::Skipped,
            _ => match state.to_ascii_uppercase().as_str() {
                "SUCCESS" | "NEUTRAL" => Self::Pass,
                "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" => Self::Fail,
                "SKIPPED" => Self::Skipped,
                _ => Self::Pending, // PENDING / IN_PROGRESS / QUEUED / unknown
            },
        }
    }
}

/// Where a check's details live: a GitHub Actions run (fetchable log), an
/// external status with only a URL, or nothing linkable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckSource {
    Run { run_id: u64 },
    External { url: String },
    None,
}

/// One CI check on a PR's head commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub name: String,
    pub state: CheckState,
    /// Human duration ("45s", "3m4s") when start+end are known.
    pub duration: Option<String>,
    pub source: CheckSource,
    /// The check's link (Actions job URL or external target), for `o` to open.
    pub url: String,
}

impl CheckRun {
    /// A failed GitHub Actions check whose log can be fetched.
    pub fn failed_run_id(&self) -> Option<u64> {
        match (self.state, &self.source) {
            (CheckState::Fail, CheckSource::Run { run_id }) => Some(*run_id),
            _ => None,
        }
    }
}

/// Raw `gh pr checks --json …` record.
#[derive(Debug, Deserialize)]
struct GhCheck {
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    bucket: String,
    #[serde(default, rename = "startedAt")]
    started_at: String,
    #[serde(default, rename = "completedAt")]
    completed_at: String,
    #[serde(default)]
    link: String,
}

/// Extract a GitHub Actions run id from a check link
/// (`…/actions/runs/<run-id>/job/<job-id>`).
pub fn extract_run_id(link: &str) -> Option<u64> {
    let rest = link.split("/actions/runs/").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn source_from_link(link: &str) -> CheckSource {
    if let Some(run_id) = extract_run_id(link) {
        CheckSource::Run { run_id }
    } else if !link.trim().is_empty() {
        CheckSource::External {
            url: link.to_string(),
        }
    } else {
        CheckSource::None
    }
}

/// Format the gap between two RFC3339 timestamps as a compact duration, or
/// `None` if either is missing/unparseable or the gap is negative (skipped
/// checks report completed-before-started).
pub fn format_duration(started_at: &str, completed_at: &str) -> Option<String> {
    let start = DateTime::parse_from_rfc3339(started_at).ok()?;
    let end = DateTime::parse_from_rfc3339(completed_at).ok()?;
    let secs = (end - start).num_seconds();
    if secs < 0 {
        return None;
    }
    Some(if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let (m, s) = (secs / 60, secs % 60);
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m{s}s")
        }
    } else {
        let (h, m) = (secs / 3600, (secs % 3600) / 60);
        format!("{h}h{m}m")
    })
}

/// Parse `gh pr checks --json name,state,bucket,startedAt,completedAt,link`
/// output into check runs. Malformed JSON yields an empty list.
pub fn parse_checks(json: &str) -> Vec<CheckRun> {
    let records: Vec<GhCheck> = serde_json::from_str(json).unwrap_or_default();
    records
        .into_iter()
        .map(|c| CheckRun {
            state: CheckState::from_gh(&c.state, &c.bucket),
            duration: format_duration(&c.started_at, &c.completed_at),
            source: source_from_link(&c.link),
            url: c.link,
            name: c.name,
        })
        .collect()
}

/// Keep only the last `LOG_TAIL_LINES` lines of a log — the tail holds the
/// error. Returns owned lines.
pub fn tail_lines(log: &str) -> Vec<String> {
    let all: Vec<&str> = log.lines().collect();
    let start = all.len().saturating_sub(LOG_TAIL_LINES);
    all[start..].iter().map(|s| s.to_string()).collect()
}

// ── async fetching ───────────────────────────────────────────────────────

use crate::gh::run as run_gh;

/// A completed failure-log fetch: which run, and its tail lines or an error.
type LogResult = (u64, Result<Vec<String>, String>);

/// Background fetcher for a PR's check list and per-run failure logs.
#[derive(Default)]
pub struct CheckFetch {
    list_rx: Option<Receiver<Result<Vec<CheckRun>, String>>>,
    log_rx: Option<Receiver<LogResult>>,
    log_cache: HashMap<u64, Vec<String>>,
}

impl CheckFetch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn the check-list fetch for `pr_number`. Replaces any in-flight one.
    pub fn start_list(&mut self, repo_path: &str, pr_number: u64) {
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            // `gh pr checks` exits non-zero when checks are pending/failing but
            // still prints the JSON — parse stdout whenever it looks like JSON,
            // regardless of exit code.
            let result = run_gh(
                &path,
                &[
                    "pr",
                    "checks",
                    &pr_number.to_string(),
                    "--json",
                    "name,state,bucket,startedAt,completedAt,link",
                ],
                LIST_TIMEOUT,
            )
            .and_then(|out| {
                if out.stdout.trim_start().starts_with('[') {
                    Ok(parse_checks(&out.stdout))
                } else if out.success {
                    Ok(Vec::new())
                } else {
                    Err(if out.stderr.is_empty() {
                        "gh pr checks failed".to_string()
                    } else {
                        out.stderr
                    })
                }
            });
            let _ = tx.send(result);
        });
        self.list_rx = Some(rx);
    }

    /// Poll the check-list fetch; `Some` once on completion.
    pub fn poll_list(&mut self) -> Option<Result<Vec<CheckRun>, String>> {
        let rx = self.list_rx.as_ref()?;
        match rx.try_recv() {
            Ok(r) => {
                self.list_rx = None;
                Some(r)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.list_rx = None;
                Some(Err("check fetch worker exited".to_string()))
            }
        }
    }

    /// A previously fetched log for `run_id`, if cached this session.
    pub fn cached_log(&self, run_id: u64) -> Option<&Vec<String>> {
        self.log_cache.get(&run_id)
    }

    /// Spawn a failure-log fetch for `run_id` unless it's cached or already in
    /// flight. Returns true if a fetch was started (caller shows "loading").
    pub fn start_log(&mut self, repo_path: &str, run_id: u64) -> bool {
        if self.log_cache.contains_key(&run_id) || self.log_rx.is_some() {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let result = run_gh(
                &path,
                &["run", "view", &run_id.to_string(), "--log-failed"],
                LOG_TIMEOUT,
            )
            .and_then(|out| {
                if out.success || !out.stdout.trim().is_empty() {
                    Ok(tail_lines(&out.stdout))
                } else {
                    Err(if out.stderr.is_empty() {
                        "could not fetch log".to_string()
                    } else {
                        out.stderr
                    })
                }
            });
            let _ = tx.send((run_id, result));
        });
        self.log_rx = Some(rx);
        true
    }

    /// Poll the failure-log fetch; caches success and returns `(run_id, result)`
    /// once on completion.
    pub fn poll_log(&mut self) -> Option<LogResult> {
        let rx = self.log_rx.as_ref()?;
        let (run_id, result) = match rx.try_recv() {
            Ok(v) => v,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.log_rx = None;
                return None;
            }
        };
        self.log_rx = None;
        if let Ok(lines) = &result {
            self.log_cache.insert(run_id, lines.clone());
        }
        Some((run_id, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parsing ──────────────────────────────────────────────────────

    #[test]
    fn parses_actions_check_shape() {
        let json = r#"[{
            "name": "build", "state": "SUCCESS", "bucket": "pass",
            "startedAt": "2026-07-18T08:00:00Z", "completedAt": "2026-07-18T08:00:45Z",
            "link": "https://github.com/o/r/actions/runs/12345/job/67890"
        }]"#;
        let checks = parse_checks(json);
        assert_eq!(checks.len(), 1);
        let c = &checks[0];
        assert_eq!(c.name, "build");
        assert_eq!(c.state, CheckState::Pass);
        assert_eq!(c.duration.as_deref(), Some("45s"));
        assert_eq!(c.source, CheckSource::Run { run_id: 12345 });
    }

    #[test]
    fn parses_external_status_context_shape() {
        // External check: non-Actions link → External, no run id.
        let json = r#"[{
            "name": "tide", "state": "PENDING", "bucket": "pending",
            "startedAt": "", "completedAt": "",
            "link": "https://prow.k8s.io/pr?query=xyz"
        }]"#;
        let checks = parse_checks(json);
        assert_eq!(checks[0].state, CheckState::Pending);
        assert_eq!(checks[0].duration, None); // no timestamps
        assert_eq!(
            checks[0].source,
            CheckSource::External {
                url: "https://prow.k8s.io/pr?query=xyz".to_string()
            }
        );
        assert!(checks[0].failed_run_id().is_none());
    }

    #[test]
    fn missing_fields_and_empty_link_degrade_gracefully() {
        let json = r#"[{"name": "x", "state": "FAILURE", "bucket": "fail"}]"#;
        let c = &parse_checks(json)[0];
        assert_eq!(c.state, CheckState::Fail);
        assert_eq!(c.duration, None);
        assert_eq!(c.source, CheckSource::None);
        // Failed but no run to fetch a log from.
        assert!(c.failed_run_id().is_none());
    }

    #[test]
    fn malformed_json_yields_empty() {
        assert!(parse_checks("not json").is_empty());
        assert!(parse_checks("").is_empty());
        assert!(parse_checks("[]").is_empty());
    }

    #[test]
    fn failed_actions_check_exposes_run_id() {
        let json = r#"[{
            "name": "test", "state": "FAILURE", "bucket": "fail",
            "link": "https://github.com/o/r/actions/runs/999/job/1"
        }]"#;
        assert_eq!(parse_checks(json)[0].failed_run_id(), Some(999));
    }

    // ── state mapping ────────────────────────────────────────────────

    #[test]
    fn state_icons_are_distinct() {
        let icons = [
            CheckState::Pass.icon(),
            CheckState::Fail.icon(),
            CheckState::Pending.icon(),
            CheckState::Skipped.icon(),
        ];
        let unique: std::collections::HashSet<_> = icons.iter().collect();
        assert_eq!(unique.len(), 4, "each state has a distinct icon");
    }

    #[test]
    fn state_mapping_prefers_bucket_then_falls_back_to_state() {
        assert_eq!(CheckState::from_gh("", "pass"), CheckState::Pass);
        assert_eq!(CheckState::from_gh("", "fail"), CheckState::Fail);
        assert_eq!(CheckState::from_gh("", "cancel"), CheckState::Fail);
        assert_eq!(CheckState::from_gh("", "pending"), CheckState::Pending);
        assert_eq!(CheckState::from_gh("", "skipping"), CheckState::Skipped);
        // Fallback to state when bucket is empty/unknown.
        assert_eq!(CheckState::from_gh("SUCCESS", ""), CheckState::Pass);
        assert_eq!(CheckState::from_gh("TIMED_OUT", ""), CheckState::Fail);
        assert_eq!(CheckState::from_gh("IN_PROGRESS", ""), CheckState::Pending);
        assert_eq!(CheckState::from_gh("SKIPPED", ""), CheckState::Skipped);
    }

    // ── run id extraction ────────────────────────────────────────────

    #[test]
    fn run_id_extraction() {
        assert_eq!(
            extract_run_id("https://github.com/cli/cli/actions/runs/29637245567/job/88061626950"),
            Some(29637245567)
        );
        assert_eq!(
            extract_run_id("https://github.com/o/r/actions/runs/42"),
            Some(42)
        );
        assert_eq!(extract_run_id("https://example.com/status/abc"), None);
        assert_eq!(extract_run_id(""), None);
    }

    // ── duration ─────────────────────────────────────────────────────

    #[test]
    fn duration_formatting() {
        let d = |a: &str, b: &str| format_duration(a, b);
        assert_eq!(
            d("2026-07-18T08:00:00Z", "2026-07-18T08:00:45Z").as_deref(),
            Some("45s")
        );
        assert_eq!(
            d("2026-07-18T08:00:00Z", "2026-07-18T08:03:04Z").as_deref(),
            Some("3m4s")
        );
        assert_eq!(
            d("2026-07-18T08:00:00Z", "2026-07-18T08:05:00Z").as_deref(),
            Some("5m")
        );
        assert_eq!(
            d("2026-07-18T08:00:00Z", "2026-07-18T09:01:00Z").as_deref(),
            Some("1h1m")
        );
        assert_eq!(
            d("2026-07-18T08:00:00Z", "2026-07-18T08:00:00Z").as_deref(),
            Some("0s")
        );
        // Negative (skipped: completed before started) → None.
        assert_eq!(d("2026-07-18T08:19:01Z", "2026-07-18T08:18:53Z"), None);
        // Missing / malformed → None.
        assert_eq!(d("", "2026-07-18T08:00:45Z"), None);
        assert_eq!(d("nonsense", "also"), None);
    }

    // ── log tail ─────────────────────────────────────────────────────

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn tail_keeps_last_lines() {
        // Under the cap: all lines kept.
        assert_eq!(tail_lines(&numbered(10)).len(), 10);
        // Exactly the cap: all kept.
        assert_eq!(tail_lines(&numbered(LOG_TAIL_LINES)).len(), LOG_TAIL_LINES);
        // Over the cap: only the last LOG_TAIL_LINES, ending at the true tail.
        let over = tail_lines(&numbered(LOG_TAIL_LINES + 50));
        assert_eq!(over.len(), LOG_TAIL_LINES);
        assert_eq!(over.first().unwrap(), "line51");
        assert_eq!(over.last().unwrap(), &format!("line{}", LOG_TAIL_LINES + 50));
        // Empty.
        assert!(tail_lines("").is_empty());
    }
}
