//! Open GitHub PR discovery via the `gh` CLI.
//!
//! Non-blocking by construction: the fetch runs on a background thread (the
//! same pattern as `NetworkManager`) and the UI polls a channel. If `gh` is
//! missing, errors, times out, or the repo has no GitHub remote, the feature is
//! silently absent — the PR map is just empty and no badges render.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Interval between PR refreshes (also the back-off after a failure).
const PR_FETCH_INTERVAL: Duration = Duration::from_secs(300);
/// Hard cap on a single `gh` invocation so a hung CLI can't leak a thread.
const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// An open pull request associated with a head branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    pub title: String,
}

/// Raw `gh pr list --json …` record.
#[derive(Debug, Deserialize)]
struct GhPr {
    number: u64,
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
}

/// Parse `gh pr list --json number,url,headRefName,title,state` output into a
/// map from head branch name to PR. Malformed JSON yields an empty map. Only
/// open PRs are kept (defensive — `gh pr list` already defaults to open). When
/// two PRs share a head branch the later record wins.
pub fn parse_pr_list(json: &str) -> HashMap<String, PrInfo> {
    let records: Vec<GhPr> = serde_json::from_str(json).unwrap_or_default();
    records
        .into_iter()
        .filter(|p| p.state.is_empty() || p.state.eq_ignore_ascii_case("open"))
        .map(|p| {
            (
                p.head_ref_name,
                PrInfo {
                    number: p.number,
                    url: p.url,
                    title: p.title,
                },
            )
        })
        .collect()
}

/// Run `gh pr list` in `repo_path`, returning open PRs by head branch. `None`
/// on any failure (gh missing, non-zero exit, timeout, non-UTF8 output).
fn fetch_open_prs(repo_path: &str) -> Option<HashMap<String, PrInfo>> {
    let output = run_gh_pr_list(repo_path)?;
    if !output.status.success() {
        return None;
    }
    let json = String::from_utf8(output.stdout).ok()?;
    Some(parse_pr_list(&json))
}

/// Spawn `gh pr list` with a timeout, killing it if it overruns `GH_TIMEOUT`.
/// Runs on a background thread, so the polling sleep never touches the UI.
fn run_gh_pr_list(repo_path: &str) -> Option<std::process::Output> {
    let mut child = Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,url,headRefName,title,state",
            "--limit",
            "100",
        ])
        .current_dir(repo_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + GH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

/// Background fetcher for open PRs. Fetches once at startup and every
/// `PR_FETCH_INTERVAL` thereafter; never blocks the UI thread.
pub struct PrFetch {
    receiver: Option<Receiver<HashMap<String, PrInfo>>>,
    last_fetch: Option<Instant>,
}

impl Default for PrFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl PrFetch {
    pub fn new() -> Self {
        Self {
            receiver: None,
            last_fetch: None,
        }
    }

    /// Make the next `maybe_start` fetch immediately, ignoring the interval.
    /// A fetch already in flight is untouched (no duplicate spawn).
    pub fn force(&mut self) {
        self.last_fetch = None;
    }

    /// Spawn a fetch when none is in flight and one is due (immediately on the
    /// first call, then on the interval).
    pub fn maybe_start(&mut self, repo_path: &str) {
        if self.receiver.is_some() {
            return;
        }
        let due = self
            .last_fetch
            .is_none_or(|t| t.elapsed() >= PR_FETCH_INTERVAL);
        if !due {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let _ = tx.send(fetch_open_prs(&path).unwrap_or_default());
        });
        self.receiver = Some(rx);
    }

    /// Poll for a completed fetch. Returns the new PR map on completion (empty
    /// on any failure), else `None`. Records the completion time so the next
    /// fetch waits a full interval — no retry storm.
    pub fn poll(&mut self) -> Option<HashMap<String, PrInfo>> {
        let rx = self.receiver.as_ref()?;
        match rx.try_recv() {
            Ok(map) => {
                self.receiver = None;
                self.last_fetch = Some(Instant::now());
                Some(map)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // Worker died without sending; back off until the next interval.
                self.receiver = None;
                self.last_fetch = Some(Instant::now());
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_prs_keyed_by_head_branch() {
        let json = r#"[
            {"number": 12, "url": "https://github.com/o/r/pull/12", "headRefName": "feat/x", "title": "Add X", "state": "OPEN"},
            {"number": 7, "url": "https://github.com/o/r/pull/7", "headRefName": "fix/y", "title": "Fix Y", "state": "OPEN"}
        ]"#;
        let map = parse_pr_list(json);
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get("feat/x"),
            Some(&PrInfo {
                number: 12,
                url: "https://github.com/o/r/pull/12".to_string(),
                title: "Add X".to_string(),
            })
        );
        assert_eq!(map.get("fix/y").map(|p| p.number), Some(7));
    }

    #[test]
    fn non_open_prs_are_dropped() {
        let json = r#"[
            {"number": 1, "url": "u1", "headRefName": "open-branch", "title": "t", "state": "OPEN"},
            {"number": 2, "url": "u2", "headRefName": "merged-branch", "title": "t", "state": "MERGED"},
            {"number": 3, "url": "u3", "headRefName": "closed-branch", "title": "t", "state": "CLOSED"}
        ]"#;
        let map = parse_pr_list(json);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("open-branch"));
        assert!(!map.contains_key("merged-branch"));
    }

    #[test]
    fn missing_state_is_treated_as_open() {
        // `gh pr list` without an explicit state filter returns open PRs; if the
        // state field is absent we keep the record rather than silently drop it.
        let json = r#"[{"number": 5, "url": "u", "headRefName": "b", "title": "t"}]"#;
        let map = parse_pr_list(json);
        assert_eq!(map.get("b").map(|p| p.number), Some(5));
    }

    #[test]
    fn malformed_json_yields_empty_map() {
        assert!(parse_pr_list("not json").is_empty());
        assert!(parse_pr_list("").is_empty());
        assert!(parse_pr_list("{}").is_empty());
    }

    #[test]
    fn empty_array_yields_empty_map() {
        assert!(parse_pr_list("[]").is_empty());
    }
}
