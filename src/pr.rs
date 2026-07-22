//! Open GitHub PR discovery via the `gh` CLI.
//!
//! Non-blocking by construction: the fetch runs on a background thread via the
//! generic [`crate::interval_fetch::IntervalFetch`] and the UI polls a channel.
//! If `gh` is missing, errors, times out, or the repo has no GitHub remote, the
//! producer returns `Err` — surfaced by `poll` so the caller can latch/toast it
//! once (issue #65), rather than silently substituting an empty PR map.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use git2::Oid;
use serde::Deserialize;

use crate::interval_fetch::IntervalFetch;

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

/// PR merge readiness, from GitHub's `mergeStateStatus`. Only the "does this
/// block the merge" distinction matters for the badge, so the many raw states
/// collapse to two. `UNSTABLE` (failing/pending checks) is deliberately *not*
/// blocking here: that condition is already carried by [`CiStatus`], so letting
/// it also block would double-count it; likewise `UNKNOWN` must never turn a
/// mergeable PR's badge yellow while GitHub is still computing the state (#88).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeState {
    /// The merge is blocked: `BLOCKED`, `DIRTY` (conflicts), `DRAFT`, `BEHIND`.
    Blocked,
    /// Not blocking: `CLEAN`, `HAS_HOOKS`, `UNSTABLE`, `UNKNOWN`, or missing.
    Clear,
}

impl MergeState {
    fn from_status(status: &str) -> Self {
        match status.to_ascii_uppercase().as_str() {
            "BLOCKED" | "DIRTY" | "DRAFT" | "BEHIND" => Self::Blocked,
            _ => Self::Clear,
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
    /// Merge readiness from `mergeStateStatus`. Combined with `review` to decide
    /// whether a green PR still reads as "blocked" (see [`PrInfo::is_merge_blocked`]).
    pub merge_state: MergeState,
    /// Any comment or review authored by someone other than the PR author.
    pub outside_activity: bool,
    /// The PR's head-branch tip commit SHA (`headRefOid`), when known. This
    /// pins the badge to exactly one row — the PR's head commit — instead of
    /// every commit whose branch label happens to match the head branch name.
    /// `None` when `gh` didn't report it (older CLI / malformed record); the
    /// renderer then falls back to head-branch-name matching.
    pub head_oid: Option<String>,
    /// The branch the PR is opened against (`baseRefName`). Base-update-merge
    /// classification (issue #55) must test against *this* branch's tip — a PR
    /// targeting `dev` back-merges `dev`, which the repo-wide trunk (`main`)
    /// does not contain (#103). `None` when `gh` didn't report it; classification
    /// then falls back to the repo-wide base branch.
    pub base_ref: Option<String>,
}

impl PrInfo {
    /// The head commit SHA parsed into a git `Oid`, when present and valid.
    pub fn head_oid(&self) -> Option<Oid> {
        self.head_oid.as_deref().and_then(|s| Oid::from_str(s).ok())
    }

    /// Whether the PR's merge is blocked — GitHub reports a blocking merge state,
    /// *or* a reviewer requested changes. This is the predicate behind the
    /// "passing checks but not actually mergeable" badge tone (#88); it is
    /// independent of CI (a failing/pending PR is coloured by its check status).
    pub fn is_merge_blocked(&self) -> bool {
        self.merge_state == MergeState::Blocked || self.review == ReviewState::ChangesRequested
    }
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

/// One PR review (only the author matters for outside-activity detection; the
/// aggregate approve/changes verdict is read from `reviewDecision` instead).
#[derive(Debug, Deserialize)]
struct Review {
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
    #[serde(default, rename = "baseRefName")]
    base_ref_name: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Option<Vec<RollupEntry>>,
    #[serde(default, rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(default, rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(default)]
    comments: Option<Vec<Comment>>,
    #[serde(default)]
    reviews: Option<Vec<Review>>,
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

/// The non-empty login of an actor, if present.
fn actor_login(actor: &Option<Actor>) -> Option<&str> {
    actor
        .as_ref()
        .map(|a| a.login.as_str())
        .filter(|l| !l.is_empty())
}

/// True if any comment *or* review was authored by someone other than the PR
/// author — i.e. the PR has been "actioned" by an outsider. Reviews are the
/// stronger signal (a review comment / requested changes without an approval),
/// so both streams are considered. Entries with a missing/empty author login
/// are skipped.
fn has_outside_activity(pr_author: &str, comments: &[Comment], reviews: &[Review]) -> bool {
    let comment_logins = comments.iter().map(|c| actor_login(&c.author));
    let review_logins = reviews.iter().map(|r| actor_login(&r.author));
    comment_logins
        .chain(review_logins)
        .flatten()
        .any(|login| login != pr_author)
}

/// Parse `gh pr list --json …` output into a map from head branch name to PR.
/// Malformed JSON yields an empty map. Only open PRs are kept (defensive — `gh
/// pr list` already defaults to open). When two PRs share a head branch the
/// later record wins.
pub fn parse_pr_list(json: &str) -> HashMap<String, PrInfo> {
    let records: Vec<GhPr> = serde_json::from_str(json).unwrap_or_default();
    records
        .into_iter()
        .filter(|p| p.state.is_empty() || p.state.eq_ignore_ascii_case("open"))
        .map(|p| {
            let ci = aggregate_ci(p.status_check_rollup.as_deref().unwrap_or_default());
            let review = ReviewState::from_decision(p.review_decision.as_deref().unwrap_or_default());
            let merge_state = MergeState::from_status(p.merge_state_status.as_deref().unwrap_or_default());
            let pr_author = p.author.as_ref().map(|a| a.login.as_str()).unwrap_or_default();
            let outside_activity = has_outside_activity(
                pr_author,
                p.comments.as_deref().unwrap_or_default(),
                p.reviews.as_deref().unwrap_or_default(),
            );
            (
                p.head_ref_name,
                PrInfo {
                    number: p.number,
                    url: p.url,
                    title: p.title,
                    ci,
                    review,
                    merge_state,
                    outside_activity,
                    head_oid: p.head_ref_oid.filter(|s| !s.is_empty()),
                    base_ref: p.base_ref_name.filter(|s| !s.is_empty()),
                },
            )
        })
        .collect()
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

/// A derived, render-ready view over the open-PR map: an index from each PR's
/// head-commit `Oid` to its `PrInfo`, plus the set of those head OIDs. Built
/// once per frame (cost is O(number of PRs), independent of commit count) so the
/// per-row graph render can answer "does this commit head a PR?" and "is this a
/// PR-merge commit?" in O(1) without any per-render scan of every commit.
pub struct PrContext<'a> {
    by_head_oid: HashMap<Oid, &'a PrInfo>,
    head_oids: HashSet<Oid>,
}

impl<'a> PrContext<'a> {
    /// Index the open PRs by head-commit OID. PRs whose `headRefOid` is absent
    /// or unparseable are simply omitted from the index (the renderer falls back
    /// to head-branch-name matching for those).
    pub fn new(open_prs: &'a HashMap<String, PrInfo>) -> Self {
        let mut by_head_oid = HashMap::new();
        let mut head_oids = HashSet::new();
        for pr in open_prs.values() {
            if let Some(oid) = pr.head_oid() {
                // On the rare collision of two PRs at the same head commit, the
                // first wins deterministically enough for a single badge.
                by_head_oid.entry(oid).or_insert(pr);
                head_oids.insert(oid);
            }
        }
        Self {
            by_head_oid,
            head_oids,
        }
    }

    /// The PR whose head commit is exactly `oid`, if any. This is the primary,
    /// data-driven mapping behind "one badge per PR, on its head commit" (#42).
    pub fn pr_for_head_commit(&self, oid: Oid) -> Option<&'a PrInfo> {
        self.by_head_oid.get(&oid).copied()
    }

    /// True when `oid` is the head commit of some open PR.
    pub fn is_pr_head(&self, oid: Oid) -> bool {
        self.head_oids.contains(&oid)
    }

    /// Whether a commit is a merge that landed a PR (#52), so its message can be
    /// dimmed. Data-driven first: a merge whose *second* parent is a known PR
    /// head is a PR merge. Falls back to the GitHub merge-commit message format
    /// for PRs no longer open (merged PRs leave `gh pr list`, so only the
    /// message survives). `is_merge` and `second_parent` come straight off the
    /// row's commit, keeping the check inside the windowed per-row render path.
    pub fn is_pr_merge(&self, is_merge: bool, second_parent: Option<Oid>, summary: &str) -> bool {
        if !is_merge {
            return false;
        }
        if second_parent.is_some_and(|p| self.is_pr_head(p)) {
            return true;
        }
        message_is_github_merge(summary)
    }
}

/// True when a commit summary line matches GitHub's default merge-commit format,
/// `Merge pull request #<n> from <owner>/<branch>`. A plain `Merge branch …`
/// (an ordinary local merge) is intentionally not matched.
pub fn message_is_github_merge(summary: &str) -> bool {
    let Some(rest) = summary.trim_start().strip_prefix("Merge pull request #") else {
        return false;
    };
    // At least one digit for the PR number, then a space (the "from …" clause).
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    !digits.is_empty() && rest[digits.len()..].starts_with(' ')
}

/// A commit whose raw subject is machinery, not authored prose, because it's
/// how a GitHub PR landed: either the default merge-commit subject (title
/// lives in the body) or the default squash-merge subject (title inlined
/// before a trailing `(#123)`). Extracts `(pr_number, pr_title)` so the
/// renderer can show "#123 <title>" instead (issue #99).
///
/// Pure and format-strict by design — no fuzzy matching, so a commit that
/// merely *mentions* a PR/issue number never gets rewritten:
/// - merge commits: `is_merge` true and `summary` starts with
///   `"Merge pull request #<digits> from"`; the title is the first non-blank
///   line *after* the subject line in `full_message`. No such line → `None`
///   (keep the raw subject rather than inventing a title).
/// - squash commits: `is_merge` false and `summary` ends with a strict
///   `<title> (#<digits>)` suffix — exactly one space before the paren, and
///   nothing after the closing paren.
pub fn pr_landed_subject(summary: &str, full_message: &str, is_merge: bool) -> Option<(u64, String)> {
    if is_merge {
        parse_merge_pr_subject(summary, full_message)
    } else {
        parse_squash_pr_subject(summary)
    }
}

/// `"Merge pull request #123 from owner/branch"` (subject) followed by a blank
/// line and the PR title (body) — GitHub's default merge-commit shape.
fn parse_merge_pr_subject(summary: &str, full_message: &str) -> Option<(u64, String)> {
    let rest = summary.trim_start().strip_prefix("Merge pull request #")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with(" from") {
        return None;
    }
    let number: u64 = digits.parse().ok()?;
    // Skip the subject line itself (full_message's first line), then take the
    // first non-blank line that follows as the title.
    let title = full_message.lines().skip(1).map(str::trim).find(|l| !l.is_empty())?;
    Some((number, title.to_string()))
}

/// `"<title> (#123)"` — GitHub's default squash-merge subject. Anchored at the
/// end: exactly one space before the paren, and the paren group holds nothing
/// but `#<digits>`, so trailing text after the paren or extra spacing before
/// it disqualifies the match rather than being fuzzily accepted.
fn parse_squash_pr_subject(summary: &str) -> Option<(u64, String)> {
    let s = summary.trim_end();
    if !s.ends_with(')') {
        return None;
    }
    let open_idx = s.rfind('(')?;
    let bytes = s.as_bytes();
    if open_idx == 0 || bytes[open_idx - 1] != b' ' {
        return None;
    }
    // Exactly one space before '(' — the byte before that space must not
    // itself be a space (rules out "title  (#123)").
    if open_idx >= 2 && bytes[open_idx - 2] == b' ' {
        return None;
    }
    let inner = &s[open_idx + 1..s.len() - 1];
    let digits = inner.strip_prefix('#')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let title = s[..open_idx - 1].trim_end();
    if title.is_empty() {
        return None;
    }
    let number: u64 = digits.parse().ok()?;
    Some((number, title.to_string()))
}

/// Build the background open-PR fetcher: `gh pr list` every [`PR_FETCH_INTERVAL`]
/// on a worker thread, routed through the generic [`IntervalFetch`].
pub fn open_pr_fetch() -> IntervalFetch<HashMap<String, PrInfo>> {
    IntervalFetch::new(PR_FETCH_INTERVAL, fetch_open_prs)
}

/// Run `gh pr list` in `repo_path`, returning open PRs by head branch. `Err`
/// (surfaced by the caller) on gh-missing, timeout, or a non-zero exit — so a
/// transient failure is observable rather than silently emptying the PR map.
fn fetch_open_prs(repo_path: &str) -> Result<HashMap<String, PrInfo>, String> {
    let out = crate::gh::run(
        repo_path,
        &[
            "pr",
            "list",
            "--json",
            "number,url,headRefName,headRefOid,baseRefName,title,state,statusCheckRollup,reviewDecision,mergeStateStatus,comments,reviews,author",
            "--limit",
            "100",
        ],
        GH_TIMEOUT,
    )?;
    if !out.success {
        return Err(if out.stderr.is_empty() {
            "gh pr list failed".to_string()
        } else {
            out.stderr
        });
    }
    Ok(parse_pr_list(&out.stdout))
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
                merge_state: MergeState::Clear,
                outside_activity: false,
                head_oid: None,
                base_ref: None,
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

    // ── merge state ──────────────────────────────────────────────────

    #[test]
    fn merge_state_blocking_statuses_map_to_blocked() {
        for s in ["BLOCKED", "DIRTY", "DRAFT", "BEHIND"] {
            let pr = one(&format!(r#","mergeStateStatus":"{s}""#));
            assert_eq!(pr.merge_state, MergeState::Blocked, "{s} should block");
            assert!(pr.is_merge_blocked(), "{s} is_merge_blocked");
        }
    }

    #[test]
    fn merge_state_non_blocking_statuses_map_to_clear() {
        // UNSTABLE (failing/pending checks) and UNKNOWN (still computing) must not
        // block — CI status already expresses UNSTABLE, and UNKNOWN must never turn
        // a mergeable PR yellow.
        for s in ["CLEAN", "HAS_HOOKS", "UNSTABLE", "UNKNOWN"] {
            let pr = one(&format!(r#","mergeStateStatus":"{s}""#));
            assert_eq!(pr.merge_state, MergeState::Clear, "{s} should be clear");
            assert!(!pr.is_merge_blocked(), "{s} not blocked");
        }
    }

    #[test]
    fn merge_state_missing_or_empty_is_clear() {
        assert_eq!(one("").merge_state, MergeState::Clear); // field absent
        assert_eq!(one(r#","mergeStateStatus":"""#).merge_state, MergeState::Clear);
        assert_eq!(one(r#","mergeStateStatus":null"#).merge_state, MergeState::Clear);
    }

    #[test]
    fn changes_requested_is_blocked_even_with_clean_merge_state() {
        // A clean mergeable state but changes requested still counts as blocked,
        // so the review verdict alone can down-tone a green badge (#88).
        let pr = one(r#","mergeStateStatus":"CLEAN","reviewDecision":"CHANGES_REQUESTED""#);
        assert_eq!(pr.merge_state, MergeState::Clear);
        assert_eq!(pr.review, ReviewState::ChangesRequested);
        assert!(pr.is_merge_blocked(), "changes-requested blocks regardless of merge state");
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

    #[test]
    fn outside_activity_detects_non_author_reviews() {
        // A review (not just an issue comment) by a non-author counts as activity.
        let reviewed = one(
            r#","author":{"login":"me"},"reviews":[{"author":{"login":"reviewer"}}]"#,
        );
        assert!(reviewed.outside_activity, "a non-author reviewed");

        let self_review = one(
            r#","author":{"login":"me"},"reviews":[{"author":{"login":"me"}}]"#,
        );
        assert!(!self_review.outside_activity, "only the author reviewed");

        // Reviews and comments are both considered.
        let mixed = one(
            r#","author":{"login":"me"},"comments":[{"author":{"login":"me"}}],"reviews":[{"author":{"login":"bot"}}]"#,
        );
        assert!(mixed.outside_activity);
    }

    // ── head OID ─────────────────────────────────────────────────────

    const OID_A: &str = "1111111111111111111111111111111111111111";
    const OID_B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn head_oid_parsed_from_json() {
        let pr = one(&format!(r#","headRefOid":"{OID_A}""#));
        assert_eq!(pr.head_oid.as_deref(), Some(OID_A));
        assert_eq!(pr.head_oid(), Some(Oid::from_str(OID_A).unwrap()));
    }

    #[test]
    fn base_ref_parsed_from_json() {
        // #103: the PR's own base branch, so back-merge classification can
        // anchor on it instead of the repo-wide trunk.
        assert_eq!(one(r#","baseRefName":"dev""#).base_ref.as_deref(), Some("dev"));
        assert_eq!(one("").base_ref, None);
        assert_eq!(one(r#","baseRefName":"""#).base_ref, None);
        assert_eq!(one(r#","baseRefName":null"#).base_ref, None);
    }

    #[test]
    fn head_oid_absent_or_empty_is_none() {
        assert_eq!(one("").head_oid, None);
        assert_eq!(one(r#","headRefOid":"""#).head_oid, None);
        assert_eq!(one(r#","headRefOid":null"#).head_oid, None);
        // A non-hex string parses to None as an Oid but is still stored verbatim.
        assert_eq!(one(r#","headRefOid":"not-a-sha""#).head_oid(), None);
    }

    // ── PrContext: PR→head-commit mapping (#42) ──────────────────────

    fn pr_with_head(number: u64, head: Option<&str>) -> PrInfo {
        PrInfo {
            number,
            url: String::new(),
            title: String::new(),
            ci: CiStatus::None,
            review: ReviewState::None,
            merge_state: MergeState::Clear,
            outside_activity: false,
            head_oid: head.map(str::to_string),
            base_ref: None,
        }
    }

    #[test]
    fn context_maps_head_commit_to_its_pr_only() {
        let mut prs = HashMap::new();
        prs.insert("feat".to_string(), pr_with_head(12, Some(OID_A)));
        let ctx = PrContext::new(&prs);
        let a = Oid::from_str(OID_A).unwrap();
        let b = Oid::from_str(OID_B).unwrap();
        // The head commit resolves to its PR; any other commit of the branch does not.
        assert_eq!(ctx.pr_for_head_commit(a).map(|p| p.number), Some(12));
        assert!(ctx.is_pr_head(a));
        assert_eq!(ctx.pr_for_head_commit(b), None);
        assert!(!ctx.is_pr_head(b));
    }

    #[test]
    fn context_omits_prs_without_a_head_oid() {
        let mut prs = HashMap::new();
        prs.insert("feat".to_string(), pr_with_head(7, None));
        let ctx = PrContext::new(&prs);
        assert_eq!(ctx.pr_for_head_commit(Oid::from_str(OID_A).unwrap()), None);
    }

    // ── PR-merge-commit detection (#52) ──────────────────────────────

    #[test]
    fn github_merge_message_pattern() {
        assert!(message_is_github_merge(
            "Merge pull request #123 from octocat/feature"
        ));
        assert!(message_is_github_merge(
            "  Merge pull request #1 from a/b" // leading whitespace tolerated
        ));
        // Not GitHub PR merges:
        assert!(!message_is_github_merge("Merge branch 'main' into feature"));
        assert!(!message_is_github_merge("Merge pull request from x")); // no number
        assert!(!message_is_github_merge("Merge pull request #12")); // no " from"
        assert!(!message_is_github_merge("fix: unrelated commit"));
        assert!(!message_is_github_merge(""));
    }

    #[test]
    fn is_pr_merge_data_driven_via_second_parent() {
        let mut prs = HashMap::new();
        prs.insert("feat".to_string(), pr_with_head(9, Some(OID_A)));
        let ctx = PrContext::new(&prs);
        let head = Oid::from_str(OID_A).unwrap();
        let other = Oid::from_str(OID_B).unwrap();

        // A merge whose 2nd parent is the PR head → PR merge, even with a plain message.
        assert!(ctx.is_pr_merge(true, Some(head), "Merge branch feat"));
        // 2nd parent is not a PR head, message not a GitHub merge → not a PR merge.
        assert!(!ctx.is_pr_merge(true, Some(other), "Merge branch feat"));
        // Not a merge at all → never a PR merge.
        assert!(!ctx.is_pr_merge(false, Some(head), "Merge pull request #9 from x/y"));
    }

    #[test]
    fn is_pr_merge_falls_back_to_message_for_closed_prs() {
        // Merged PRs leave `gh pr list`, so the OID index is empty — the message
        // pattern still identifies the merge commit.
        let empty = HashMap::new();
        let ctx = PrContext::new(&empty);
        let p = Oid::from_str(OID_A).unwrap();
        assert!(ctx.is_pr_merge(true, Some(p), "Merge pull request #42 from o/b"));
        assert!(!ctx.is_pr_merge(true, Some(p), "Merge branch 'x'"));
    }

    // ── pr_refresh_summary ───────────────────────────────────────────

    fn pr_info(number: u64, ci: CiStatus) -> PrInfo {
        PrInfo {
            number,
            url: String::new(),
            title: String::new(),
            ci,
            review: ReviewState::None,
            merge_state: MergeState::Clear,
            outside_activity: false,
            head_oid: None,
            base_ref: None,
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

    // ── pr_landed_subject ─────────────────────────────────────────────

    #[test]
    fn merge_subject_with_body_title_extracts_number_and_title() {
        let summary = "Merge pull request #123 from owner/feat-branch";
        let full = "Merge pull request #123 from owner/feat-branch\n\nAdd the frobnicator\n";
        assert_eq!(
            pr_landed_subject(summary, full, true),
            Some((123, "Add the frobnicator".to_string()))
        );
    }

    #[test]
    fn merge_subject_with_no_body_title_returns_none() {
        // No non-blank line after the subject — don't invent a title.
        let summary = "Merge pull request #123 from owner/feat-branch";
        let full = "Merge pull request #123 from owner/feat-branch\n\n\n";
        assert_eq!(pr_landed_subject(summary, full, true), None);

        let full_no_body_at_all = "Merge pull request #123 from owner/feat-branch";
        assert_eq!(pr_landed_subject(summary, full_no_body_at_all, true), None);
    }

    #[test]
    fn merge_subject_title_is_trimmed() {
        let summary = "Merge pull request #7 from owner/x";
        let full = "Merge pull request #7 from owner/x\n\n   Padded title   \nmore body\n";
        assert_eq!(
            pr_landed_subject(summary, full, true),
            Some((7, "Padded title".to_string()))
        );
    }

    #[test]
    fn squash_subject_extracts_number_and_title() {
        let summary = "Some title (#456)";
        assert_eq!(
            pr_landed_subject(summary, "irrelevant", false),
            Some((456, "Some title".to_string()))
        );
    }

    #[test]
    fn squash_subject_with_extra_text_after_paren_returns_none() {
        let summary = "Some title (#456) extra text";
        assert_eq!(pr_landed_subject(summary, "irrelevant", false), None);
    }

    #[test]
    fn squash_subject_with_double_space_before_paren_returns_none() {
        let summary = "Some title  (#456)";
        assert_eq!(pr_landed_subject(summary, "irrelevant", false), None);
    }

    #[test]
    fn squash_subject_with_empty_title_returns_none() {
        let summary = "(#456)";
        assert_eq!(pr_landed_subject(summary, "irrelevant", false), None);
    }

    #[test]
    fn subject_merely_referencing_an_issue_mid_line_returns_none() {
        // No trailing "(#n)" suffix — mid-sentence mention, not the squash shape.
        let summary = "Fix bug referenced in #123 for good measure";
        assert_eq!(pr_landed_subject(summary, "irrelevant", false), None);

        // Also not a merge subject.
        assert_eq!(pr_landed_subject(summary, "irrelevant", true), None);
    }

    #[test]
    fn non_merge_prefix_is_never_treated_as_a_merge_subject() {
        let summary = "Merge branch 'main' into feature";
        assert_eq!(pr_landed_subject(summary, "Merge branch 'main' into feature\n\nsome body", true), None);
    }
}
