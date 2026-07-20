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

/// Aggregate CI status of a PR's head commit, from `statusCheckRollup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiStatus {
    /// No checks reported.
    None,
    /// All checks succeeded (SUCCESS/SKIPPED/NEUTRAL).
    Pass,
    /// At least one check is still running / queued and none failed.
    Pending,
    /// At least one check failed (FAILURE/ERROR/CANCELLED/TIMED_OUT).
    Fail,
}

impl CiStatus {
    /// Short label for a toast, e.g. "CI passing".
    pub fn short_label(self) -> &'static str {
        match self {
            Self::None => "no CI",
            Self::Pass => "CI passing",
            Self::Pending => "CI running",
            Self::Fail => "CI failing",
        }
    }
}

/// PR review decision, from `reviewDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    None,
    Approved,
    ChangesRequested,
}

impl ReviewState {
    fn from_decision(decision: &str) -> Self {
        match decision.to_ascii_uppercase().as_str() {
            "APPROVED" => Self::Approved,
            "CHANGES_REQUESTED" => Self::ChangesRequested,
            _ => Self::None,
        }
    }
}

/// An open pull request associated with a head branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub ci: CiStatus,
    pub review: ReviewState,
    /// Any comment authored by someone other than the PR author.
    pub outside_activity: bool,
    /// The PR's head commit (`headRefOid`), when `gh` reported a parseable one.
    /// Used to anchor the graph badge to exactly one row — the commit the PR's
    /// head branch points at — instead of every ref that shares the head branch
    /// name (a local `feat` and `origin/feat` on different commits would
    /// otherwise both render a badge). `None` falls back to branch-name matching.
    pub head_oid: Option<git2::Oid>,
}

/// One `statusCheckRollup` entry. Both shapes appear: `CheckRun`
/// (`status`/`conclusion`) and `StatusContext` (`state`); all fields optional.
#[derive(Debug, Deserialize)]
struct RollupEntry {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// A GitHub actor (comment/PR author).
#[derive(Debug, Deserialize)]
struct Actor {
    #[serde(default)]
    login: String,
}

/// One PR comment (only the author matters here).
#[derive(Debug, Deserialize)]
struct Comment {
    #[serde(default)]
    author: Option<Actor>,
}

/// Raw `gh pr list --json …` record. Heavier fields are `Option`/defaulted so a
/// `null` or missing value degrades gracefully instead of failing the parse.
#[derive(Debug, Deserialize)]
struct GhPr {
    number: u64,
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(default, rename = "headRefOid")]
    head_ref_oid: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Option<Vec<RollupEntry>>,
    #[serde(default, rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(default)]
    comments: Option<Vec<Comment>>,
    #[serde(default)]
    author: Option<Actor>,
}

/// Map a single verdict token to pass / pending / fail.
fn classify_verdict(verdict: &str) -> CiStatus {
    match verdict.to_ascii_uppercase().as_str() {
        "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" => CiStatus::Fail,
        "SUCCESS" | "SKIPPED" | "NEUTRAL" => CiStatus::Pass,
        // PENDING / IN_PROGRESS / QUEUED / empty / unknown → not-yet-green.
        _ => CiStatus::Pending,
    }
}

/// Classify one rollup entry into pass / pending / fail (order-independent, so
/// the aggregate doesn't depend on entry order). `StatusContext` carries its
/// verdict in `state`; a `CheckRun` that hasn't COMPLETED is pending regardless,
/// otherwise its `conclusion` decides (empty/null ⇒ pending).
fn classify_entry(e: &RollupEntry) -> CiStatus {
    if let Some(state) = e.state.as_deref().filter(|s| !s.is_empty()) {
        return classify_verdict(state);
    }
    if let Some(status) = e.status.as_deref().filter(|s| !s.is_empty()) {
        if !status.eq_ignore_ascii_case("COMPLETED") {
            return CiStatus::Pending;
        }
    }
    classify_verdict(e.conclusion.as_deref().unwrap_or_default())
}

/// Aggregate a rollup: any fail ⇒ Fail; else any pending ⇒ Pending; else Pass;
/// empty/missing ⇒ None.
fn aggregate_ci(rollup: &[RollupEntry]) -> CiStatus {
    if rollup.is_empty() {
        return CiStatus::None;
    }
    let mut pending = false;
    for e in rollup {
        match classify_entry(e) {
            CiStatus::Fail => return CiStatus::Fail,
            CiStatus::Pending => pending = true,
            CiStatus::Pass | CiStatus::None => {}
        }
    }
    if pending {
        CiStatus::Pending
    } else {
        CiStatus::Pass
    }
}

/// True if any comment was authored by someone other than the PR author.
/// Comments with a missing/empty author login are skipped.
fn has_outside_activity(pr_author: &str, comments: &[Comment]) -> bool {
    comments.iter().any(|c| {
        c.author
            .as_ref()
            .map(|a| a.login.as_str())
            .filter(|l| !l.is_empty())
            .is_some_and(|login| login != pr_author)
    })
}

/// Open PRs plus the head-branch names of merged PRs. Fetched together so the
/// graph can badge open PRs and dim branches already merged via a PR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrData {
    /// Open PRs, keyed by head branch name.
    pub open: HashMap<String, PrInfo>,
    /// Head branch names of PRs that were merged (state MERGED).
    pub merged_branches: std::collections::HashSet<String>,
}

/// Convert one raw record into `(head_branch, PrInfo)`.
fn pr_info_from(p: GhPr) -> (String, PrInfo) {
    let ci = aggregate_ci(p.status_check_rollup.as_deref().unwrap_or_default());
    let review = ReviewState::from_decision(p.review_decision.as_deref().unwrap_or_default());
    let pr_author = p.author.as_ref().map(|a| a.login.as_str()).unwrap_or_default();
    let outside_activity =
        has_outside_activity(pr_author, p.comments.as_deref().unwrap_or_default());
    let head_oid = p
        .head_ref_oid
        .as_deref()
        .and_then(|s| git2::Oid::from_str(s).ok());
    (
        p.head_ref_name,
        PrInfo {
            number: p.number,
            url: p.url,
            title: p.title,
            ci,
            review,
            outside_activity,
            head_oid,
        },
    )
}

/// Parse `gh pr list --json …` output into a map from head branch name to open
/// PR. Malformed JSON yields an empty map. Only open PRs are kept (a missing
/// state is treated as open — `gh pr list` defaults to open). When two PRs share
/// a head branch the later record wins.
pub fn parse_pr_list(json: &str) -> HashMap<String, PrInfo> {
    let records: Vec<GhPr> = serde_json::from_str(json).unwrap_or_default();
    records
        .into_iter()
        .filter(|p| p.state.is_empty() || p.state.eq_ignore_ascii_case("open"))
        .map(pr_info_from)
        .collect()
}

/// Parse `gh pr list --state all …` output into open PRs and merged head
/// branches. Open PRs (or state-less records) populate `open`; records with
/// state MERGED contribute their head branch to `merged_branches`. Closed
/// (unmerged) PRs are ignored — a closed-without-merge branch isn't "merged via
/// PR". Malformed JSON yields empty data.
pub fn parse_pr_data(json: &str) -> PrData {
    let records: Vec<GhPr> = serde_json::from_str(json).unwrap_or_default();
    let mut data = PrData::default();
    for p in records {
        if p.state.eq_ignore_ascii_case("merged") {
            data.merged_branches.insert(p.head_ref_name);
        } else if p.state.is_empty() || p.state.eq_ignore_ascii_case("open") {
            let (branch, info) = pr_info_from(p);
            data.open.insert(branch, info);
        }
        // CLOSED (unmerged) → ignored.
    }
    data
}

/// Summarize what changed between two open-PR maps, for a refresh toast, or
/// `None` when nothing worth surfacing changed. Surfaces a newly-appeared PR or
/// a CI-status change on an existing PR; a no-op refresh (equal maps, or only
/// unrelated field changes) returns `None`.
pub fn pr_refresh_summary(
    old: &HashMap<String, PrInfo>,
    new: &HashMap<String, PrInfo>,
) -> Option<String> {
    let mut new_pr: Option<u64> = None;
    let mut new_count = 0usize;
    let mut ci_change: Option<(u64, CiStatus)> = None;
    let mut ci_count = 0usize;
    for (branch, pr) in new {
        match old.get(branch) {
            None => {
                new_count += 1;
                new_pr.get_or_insert(pr.number);
            }
            Some(o) if o.ci != pr.ci => {
                ci_count += 1;
                ci_change.get_or_insert((pr.number, pr.ci));
            }
            _ => {}
        }
    }
    if new_count == 0 && ci_count == 0 {
        return None;
    }
    // A single specific event gets precise text; multiple get a count (which is
    // order-independent, since HashMap iteration order isn't stable).
    Some(match (new_count, ci_count) {
        (1, 0) => format!("New PR #{}", new_pr.unwrap()),
        (0, 1) => {
            let (n, ci) = ci_change.unwrap();
            format!("PR #{n}: {}", ci.short_label())
        }
        _ => {
            let mut parts = Vec::new();
            if new_count > 0 {
                parts.push(format!("{new_count} new"));
            }
            if ci_count > 0 {
                parts.push(format!(
                    "{ci_count} CI update{}",
                    if ci_count == 1 { "" } else { "s" }
                ));
            }
            format!("PRs: {}", parts.join(", "))
        }
    })
}

/// Run `gh pr list --state all` in `repo_path`, returning open PRs plus merged
/// head branches. `None` on any failure (gh missing, non-zero exit, timeout,
/// non-UTF8 output).
fn fetch_pr_data(repo_path: &str) -> Option<PrData> {
    let output = run_gh_pr_list(repo_path)?;
    if !output.status.success() {
        return None;
    }
    let json = String::from_utf8(output.stdout).ok()?;
    Some(parse_pr_data(&json))
}

/// Spawn `gh pr list --state all` with a timeout, killing it if it overruns
/// `GH_TIMEOUT`. Runs on a background thread, so the polling sleep never touches
/// the UI. `--state all` returns open + closed + merged; the parser splits them.
fn run_gh_pr_list(repo_path: &str) -> Option<std::process::Output> {
    let mut child = Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "all",
            "--json",
            "number,url,headRefName,headRefOid,title,state,statusCheckRollup,reviewDecision,comments,author",
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

/// Background fetcher for PR data (open PRs + merged head branches). Fetches
/// once at startup and every `PR_FETCH_INTERVAL` thereafter; never blocks the UI
/// thread.
pub struct PrFetch {
    receiver: Option<Receiver<PrData>>,
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
            let _ = tx.send(fetch_pr_data(&path).unwrap_or_default());
        });
        self.receiver = Some(rx);
    }

    /// Poll for a completed fetch. Returns the new PR data on completion (empty
    /// on any failure), else `None`. Records the completion time so the next
    /// fetch waits a full interval — no retry storm.
    pub fn poll(&mut self) -> Option<PrData> {
        let rx = self.receiver.as_ref()?;
        match rx.try_recv() {
            Ok(data) => {
                self.receiver = None;
                self.last_fetch = Some(Instant::now());
                Some(data)
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
                ci: CiStatus::None,
                review: ReviewState::None,
                outside_activity: false,
                head_oid: None,
            })
        );
        assert_eq!(map.get("fix/y").map(|p| p.number), Some(7));
    }

    #[test]
    fn head_ref_oid_is_parsed_when_present_and_none_when_absent_or_bad() {
        let json = r#"[
            {"number": 1, "url": "u", "headRefName": "a", "headRefOid": "0123456789012345678901234567890123456789", "state": "OPEN"},
            {"number": 2, "url": "u", "headRefName": "b", "headRefOid": "not-a-sha", "state": "OPEN"},
            {"number": 3, "url": "u", "headRefName": "c", "state": "OPEN"}
        ]"#;
        let map = parse_pr_list(json);
        assert_eq!(
            map.get("a").unwrap().head_oid,
            Some(git2::Oid::from_str("0123456789012345678901234567890123456789").unwrap()),
            "valid headRefOid parses"
        );
        assert_eq!(map.get("b").unwrap().head_oid, None, "unparseable oid → None");
        assert_eq!(map.get("c").unwrap().head_oid, None, "missing oid → None");
    }

    #[test]
    fn parse_pr_data_splits_open_and_merged_ignoring_closed() {
        let json = r#"[
            {"number": 1, "url": "u", "headRefName": "open-branch", "title": "t", "state": "OPEN"},
            {"number": 2, "url": "u", "headRefName": "merged-branch", "title": "t", "state": "MERGED"},
            {"number": 3, "url": "u", "headRefName": "closed-branch", "title": "t", "state": "CLOSED"}
        ]"#;
        let data = parse_pr_data(json);
        assert_eq!(data.open.len(), 1);
        assert!(data.open.contains_key("open-branch"));
        assert!(data.merged_branches.contains("merged-branch"));
        assert!(
            !data.merged_branches.contains("closed-branch"),
            "closed-without-merge is not a merged branch"
        );
        assert!(!data.open.contains_key("merged-branch"));
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

    // ── CI aggregation ───────────────────────────────────────────────

    /// Parse a single-PR fixture and return its PrInfo.
    fn one(extra_fields: &str) -> PrInfo {
        let json = format!(
            r#"[{{"number":1,"url":"u","headRefName":"b","title":"t","state":"OPEN"{extra_fields}}}]"#
        );
        parse_pr_list(&json).remove("b").expect("PR present")
    }

    #[test]
    fn ci_checkrun_shape_mixed_fail_and_pending_is_fail() {
        // Real CheckRun shape (status/conclusion). Fail beats pending.
        let pr = one(
            r#","statusCheckRollup":[
                {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":null},
                {"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"},
                {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}
            ]"#,
        );
        assert_eq!(pr.ci, CiStatus::Fail);
    }

    #[test]
    fn ci_checkrun_running_is_pending() {
        // A null conclusion (still running) with no failures → Pending.
        let pr = one(
            r#","statusCheckRollup":[
                {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
                {"__typename":"CheckRun","status":"QUEUED","conclusion":null}
            ]"#,
        );
        assert_eq!(pr.ci, CiStatus::Pending);
    }

    #[test]
    fn ci_statuscontext_shape_is_handled() {
        // Legacy StatusContext shape (state), as emitted by e.g. Prow/tide.
        let pending = one(r#","statusCheckRollup":[{"__typename":"StatusContext","state":"PENDING"}]"#);
        assert_eq!(pending.ci, CiStatus::Pending);
        let failed = one(r#","statusCheckRollup":[{"__typename":"StatusContext","state":"FAILURE"}]"#);
        assert_eq!(failed.ci, CiStatus::Fail);
        let ok = one(r#","statusCheckRollup":[{"__typename":"StatusContext","state":"SUCCESS"}]"#);
        assert_eq!(ok.ci, CiStatus::Pass);
    }

    #[test]
    fn ci_all_success_is_pass_including_skipped_and_neutral() {
        let pr = one(
            r#","statusCheckRollup":[
                {"status":"COMPLETED","conclusion":"SUCCESS"},
                {"status":"COMPLETED","conclusion":"SKIPPED"},
                {"status":"COMPLETED","conclusion":"NEUTRAL"}
            ]"#,
        );
        assert_eq!(pr.ci, CiStatus::Pass);
    }

    #[test]
    fn ci_fail_precedence_over_everything() {
        // FAILURE / ERROR / CANCELLED / TIMED_OUT all count as Fail.
        for c in ["FAILURE", "ERROR", "CANCELLED", "TIMED_OUT"] {
            let pr = one(&format!(
                r#","statusCheckRollup":[
                    {{"status":"COMPLETED","conclusion":"SUCCESS"}},
                    {{"status":"COMPLETED","conclusion":"PENDING"}},
                    {{"status":"COMPLETED","conclusion":"{c}"}}
                ]"#
            ));
            assert_eq!(pr.ci, CiStatus::Fail, "{c} should be Fail");
        }
    }

    #[test]
    fn ci_empty_or_missing_rollup_is_none() {
        assert_eq!(one(r#","statusCheckRollup":[]"#).ci, CiStatus::None);
        assert_eq!(one(r#","statusCheckRollup":null"#).ci, CiStatus::None);
        assert_eq!(one("").ci, CiStatus::None); // field absent entirely
    }

    // ── review decision ──────────────────────────────────────────────

    #[test]
    fn review_decision_mapping() {
        assert_eq!(one(r#","reviewDecision":"APPROVED""#).review, ReviewState::Approved);
        assert_eq!(
            one(r#","reviewDecision":"CHANGES_REQUESTED""#).review,
            ReviewState::ChangesRequested
        );
        assert_eq!(one(r#","reviewDecision":"REVIEW_REQUIRED""#).review, ReviewState::None);
        assert_eq!(one(r#","reviewDecision":"""#).review, ReviewState::None);
        assert_eq!(one(r#","reviewDecision":null"#).review, ReviewState::None);
        assert_eq!(one("").review, ReviewState::None);
    }

    // ── outside activity ─────────────────────────────────────────────

    #[test]
    fn outside_activity_detects_non_author_comments() {
        let author_only = one(
            r#","author":{"login":"me"},"comments":[{"author":{"login":"me"}},{"author":{"login":"me"}}]"#,
        );
        assert!(!author_only.outside_activity, "only the author commented");

        let mixed = one(
            r#","author":{"login":"me"},"comments":[{"author":{"login":"me"}},{"author":{"login":"someone"}}]"#,
        );
        assert!(mixed.outside_activity, "a non-author commented");

        let none = one(r#","author":{"login":"me"},"comments":[]"#);
        assert!(!none.outside_activity, "zero comments");
    }

    #[test]
    fn outside_activity_skips_malformed_comment_entries() {
        // Missing author / empty login entries are skipped, not errored.
        let pr = one(
            r#","author":{"login":"me"},"comments":[{},{"author":{"login":""}},{"author":{"login":"me"}}]"#,
        );
        assert!(!pr.outside_activity);
        // Whole parse still succeeds and yields the PR.
        assert_eq!(pr.number, 1);
    }

    // ── pr_refresh_summary ───────────────────────────────────────────

    fn pr_info(number: u64, ci: CiStatus) -> PrInfo {
        PrInfo {
            number,
            url: String::new(),
            title: String::new(),
            ci,
            review: ReviewState::None,
            outside_activity: false,
            head_oid: None,
        }
    }

    fn map(entries: &[(&str, u64, CiStatus)]) -> HashMap<String, PrInfo> {
        entries
            .iter()
            .map(|(b, n, ci)| (b.to_string(), pr_info(*n, *ci)))
            .collect()
    }

    #[test]
    fn refresh_summary_no_op_is_none() {
        let m = map(&[("a", 1, CiStatus::Pass)]);
        assert_eq!(pr_refresh_summary(&m, &m.clone()), None);
        // Unrelated field change (not CI) is not surfaced.
        let mut changed = m.clone();
        changed.get_mut("a").unwrap().outside_activity = true;
        assert_eq!(pr_refresh_summary(&m, &changed), None);
    }

    #[test]
    fn refresh_summary_new_pr() {
        let old = map(&[("a", 1, CiStatus::Pass)]);
        let new = map(&[("a", 1, CiStatus::Pass), ("b", 7, CiStatus::None)]);
        assert_eq!(pr_refresh_summary(&old, &new).as_deref(), Some("New PR #7"));
    }

    #[test]
    fn refresh_summary_ci_change() {
        let old = map(&[("a", 3, CiStatus::Pending)]);
        let new = map(&[("a", 3, CiStatus::Fail)]);
        assert_eq!(
            pr_refresh_summary(&old, &new).as_deref(),
            Some("PR #3: CI failing")
        );
    }

    #[test]
    fn refresh_summary_multiple_uses_counts() {
        let old = map(&[("a", 1, CiStatus::Pending)]);
        let new = map(&[
            ("a", 1, CiStatus::Pass),  // CI change
            ("b", 2, CiStatus::None),  // new
            ("c", 3, CiStatus::None),  // new
        ]);
        // Order-independent count summary.
        assert_eq!(
            pr_refresh_summary(&old, &new).as_deref(),
            Some("PRs: 2 new, 1 CI update")
        );
    }
}
