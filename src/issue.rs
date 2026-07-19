//! GitHub Issues discovery via the `gh` CLI.
//!
//! On-demand and background-fetched (the `CheckFetch`/`PrThreadFetch` pattern,
//! not `PrFetch`'s periodic poll): the issue list is fetched when the popup
//! opens, a per-number detail is fetched when an issue is opened (cached for the
//! session), and the label set is fetched once for the label picker. Nothing
//! here blocks the UI thread; failures resolve to an `Err(String)` the caller
//! renders inline — never a hang.
//!
//! Everything above `IssueFetch` is pure and unit-tested.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::gh;

/// Timeout for the issue-list query.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for the blocked-by (issue dependency) query.
const BLOCKED_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for a single issue's detail (body + comments can be large).
const DETAIL_TIMEOUT: Duration = Duration::from_secs(20);
/// Timeout for the label-list query.
const LABEL_TIMEOUT: Duration = Duration::from_secs(15);

/// Open/closed state of an issue. gh emits these uppercase (`OPEN`/`CLOSED`);
/// parsing is case-insensitive and treats anything unrecognized as open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    Open,
    Closed,
}

impl IssueState {
    fn from_str(s: &str) -> Self {
        if s.eq_ignore_ascii_case("closed") {
            Self::Closed
        } else {
            Self::Open
        }
    }

    /// Short label for a toast/header, e.g. "open".
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// Which issues to list, mapped to gh's `--state` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueFilter {
    Open,
    Closed,
    All,
}

impl IssueFilter {
    /// The value passed to `gh issue list --state`.
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::All => "all",
        }
    }

    /// Short label for the filter tab.
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Closed => "Closed",
            Self::All => "All",
        }
    }

    /// Next filter in the Open → Closed → All → Open cycle.
    pub fn next(self) -> Self {
        match self {
            Self::Open => Self::Closed,
            Self::Closed => Self::All,
            Self::All => Self::Open,
        }
    }

    pub const ALL: [IssueFilter; 3] = [Self::Open, Self::Closed, Self::All];
}

/// A label on an issue, with its hex color (no leading `#`, as gh emits it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueLabel {
    pub name: String,
    pub color: String,
}

/// A row in the issue list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueInfo {
    pub number: u64,
    pub title: String,
    pub state: IssueState,
    pub labels: Vec<IssueLabel>,
    pub assignees: Vec<String>,
    pub author: String,
    pub updated_at: String,
    pub url: String,
}

/// Client-side view filter over an already-fetched issue list. Orthogonal to the
/// gh-side `IssueFilter` (open/closed/all): this narrows the fetched rows without
/// a refetch. `labels` is a set of selected label names — an issue passes when it
/// carries at least one of them (empty = no label filtering). `unblocked_only`
/// hides issues that have at least one open blocking issue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueViewFilter {
    pub labels: Vec<String>,
    pub unblocked_only: bool,
}

impl IssueViewFilter {
    /// Whether any client-side narrowing is active (drives header display).
    pub fn is_active(&self) -> bool {
        !self.labels.is_empty() || self.unblocked_only
    }
}

/// Whether a single issue passes the client-side view filter. `blocked` is
/// whether this issue has an open blocking issue (per the dependency data);
/// when the data is unavailable every issue is treated as unblocked.
pub fn issue_matches(issue: &IssueInfo, filter: &IssueViewFilter, blocked: bool) -> bool {
    if !filter.labels.is_empty() {
        let has = issue
            .labels
            .iter()
            .any(|l| filter.labels.iter().any(|f| f == &l.name));
        if !has {
            return false;
        }
    }
    if filter.unblocked_only && blocked {
        return false;
    }
    true
}

/// Indices into `issues` (in order) that pass the view filter. `blocked` is the
/// set of issue numbers with at least one open blocker. Shared by the widget
/// (display) and the handlers (selection/navigation) so both agree on the set.
pub fn visible_issues(
    issues: &[IssueInfo],
    filter: &IssueViewFilter,
    blocked: &HashSet<u64>,
) -> Vec<usize> {
    issues
        .iter()
        .enumerate()
        .filter(|(_, i)| issue_matches(i, filter, blocked.contains(&i.number)))
        .map(|(idx, _)| idx)
        .collect()
}

/// One comment on an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueComment {
    pub author: String,
    pub created_at: String,
    pub body: String,
}

/// Full detail for a single issue (body + comments), for the detail popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueDetail {
    pub number: u64,
    pub title: String,
    pub state: IssueState,
    /// Why a closed issue was closed (`COMPLETED`/`NOT_PLANNED`); `None` when open.
    pub state_reason: Option<String>,
    pub body: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    pub labels: Vec<IssueLabel>,
    pub assignees: Vec<String>,
    pub comments: Vec<IssueComment>,
    pub url: String,
}

// ── JSON parsing ───────────────────────────────────────────────────────────

/// A GitHub actor. Only the login matters here.
#[derive(Debug, Deserialize)]
struct Actor {
    #[serde(default)]
    login: String,
}

/// Login of an actor, or "ghost" when missing/empty (mirrors `pr_thread`).
fn actor_login(a: &Option<Actor>) -> String {
    a.as_ref()
        .map(|a| a.login.clone())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "ghost".to_string())
}

/// Raw `label` object from `--json labels`/`gh label list`. Heavy fields are
/// defaulted so a missing/null value degrades instead of failing the parse.
#[derive(Debug, Deserialize)]
struct GhLabel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    color: String,
}

impl From<GhLabel> for IssueLabel {
    fn from(l: GhLabel) -> Self {
        IssueLabel {
            name: l.name,
            color: l.color,
        }
    }
}

fn labels_from(raw: Vec<GhLabel>) -> Vec<IssueLabel> {
    raw.into_iter().map(IssueLabel::from).collect()
}

fn assignees_from(raw: Vec<Actor>) -> Vec<String> {
    raw.into_iter()
        .map(|a| a.login)
        .filter(|l| !l.is_empty())
        .collect()
}

/// Raw `gh issue list --json …` record.
#[derive(Debug, Deserialize)]
struct GhIssue {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(default)]
    assignees: Vec<Actor>,
    #[serde(default)]
    author: Option<Actor>,
    #[serde(default, rename = "updatedAt")]
    updated_at: String,
    #[serde(default)]
    url: String,
}

/// Raw `gh issue view --json …` comment record.
#[derive(Debug, Deserialize)]
struct GhComment {
    #[serde(default)]
    author: Option<Actor>,
    #[serde(default, rename = "createdAt")]
    created_at: String,
    // `Option` so an explicit JSON `null` (gh's empty body) parses as absent.
    #[serde(default)]
    body: Option<String>,
}

/// Raw `gh issue view --json …` record.
#[derive(Debug, Deserialize)]
struct GhIssueDetail {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default, rename = "stateReason")]
    state_reason: Option<String>,
    // `Option` so an explicit JSON `null` (gh's empty body) parses as absent.
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    author: Option<Actor>,
    #[serde(default, rename = "createdAt")]
    created_at: String,
    #[serde(default, rename = "updatedAt")]
    updated_at: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(default)]
    assignees: Vec<Actor>,
    #[serde(default)]
    comments: Vec<GhComment>,
    #[serde(default)]
    url: String,
}

/// Parse `gh issue list --json …` output. `Err` on malformed JSON; an empty
/// array yields an empty list.
pub fn parse_issue_list(json: &str) -> Result<Vec<IssueInfo>, String> {
    let records: Vec<GhIssue> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(records
        .into_iter()
        .map(|i| IssueInfo {
            number: i.number,
            title: i.title,
            state: IssueState::from_str(&i.state),
            labels: labels_from(i.labels),
            assignees: assignees_from(i.assignees),
            author: actor_login(&i.author),
            updated_at: i.updated_at,
            url: i.url,
        })
        .collect())
}

/// Parse `gh issue view N --json …` output. `Err` on malformed JSON.
pub fn parse_issue_detail(json: &str) -> Result<IssueDetail, String> {
    let i: GhIssueDetail = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(IssueDetail {
        number: i.number,
        title: i.title,
        state: IssueState::from_str(&i.state),
        // gh emits an empty string (not null) for an open issue's reason.
        state_reason: i.state_reason.filter(|r| !r.is_empty()),
        body: i.body.unwrap_or_default(),
        author: actor_login(&i.author),
        created_at: i.created_at,
        updated_at: i.updated_at,
        labels: labels_from(i.labels),
        assignees: assignees_from(i.assignees),
        comments: i
            .comments
            .into_iter()
            .map(|c| IssueComment {
                author: actor_login(&c.author),
                created_at: c.created_at,
                body: c.body.unwrap_or_default(),
            })
            .collect(),
        url: i.url,
    })
}

/// Parse `gh label list --json name,color` output. `Err` on malformed JSON.
pub fn parse_label_list(json: &str) -> Result<Vec<IssueLabel>, String> {
    let records: Vec<GhLabel> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(labels_from(records))
}

// ── blocked-by (issue dependencies) ──────────────────────────────────────────

/// GraphQL query for the repo's open issues, their native `blockedBy`
/// dependencies (with each blocker's state), and their bodies (so a checklist /
/// "blocked by #N" reference to another *open* issue also counts as a blocker
/// when native dependencies aren't used). Single-line so it passes cleanly as a
/// `-f query=…` argument to `gh api graphql`.
const BLOCKED_QUERY: &str = "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){issues(first:100,states:OPEN){nodes{number body blockedBy(first:20){nodes{number state}}}}}}";

/// A raw blocker edge: the blocking issue's number and whether it is open.
#[derive(Debug, Clone)]
struct RawBlocker {
    open: bool,
}

/// A parsed issue node from the dependency query, before the blocked-set is
/// derived. Kept separate from JSON so the derivation is pure and unit-testable.
#[derive(Debug, Clone)]
struct RawBlockedIssue {
    number: u64,
    body: String,
    native: Vec<RawBlocker>,
}

/// Extract issue numbers referenced as blockers in a body. Case-insensitive,
/// matching `blocked by #N`, `depends on #N`, and GitHub task-list rows
/// (`- [ ] #N`). Cross-repo references (`owner/repo#N`) are ignored — only bare
/// `#N` in this repo count. The caller filters these to *open* issues.
pub fn blockers_in_body(body: &str) -> Vec<u64> {
    let lower = body.to_lowercase();
    let mut out: Vec<u64> = Vec::new();
    let mut push = |n: u64| {
        if !out.contains(&n) {
            out.push(n);
        }
    };
    // Phrase-anchored references anywhere in a line.
    for anchor in ["blocked by", "depends on"] {
        let mut rest = lower.as_str();
        while let Some(pos) = rest.find(anchor) {
            let after = &rest[pos + anchor.len()..];
            if let Some(n) = leading_issue_ref(after) {
                push(n);
            }
            rest = &rest[pos + anchor.len()..];
        }
    }
    // Unchecked task-list rows: `- [ ] #N …`.
    for line in lower.lines() {
        let t = line.trim_start();
        for marker in ["- [ ]", "* [ ]", "- [ ] ", "[ ]"] {
            if let Some(after) = t.strip_prefix(marker) {
                if let Some(n) = leading_issue_ref(after) {
                    push(n);
                }
                break;
            }
        }
    }
    out
}

/// Parse the first `#N` issue reference at (or just after leading whitespace of)
/// the start of `s`, rejecting a cross-repo `owner/repo#N` (a `/` or word char
/// immediately before the `#`). Returns the number, or `None`.
fn leading_issue_ref(s: &str) -> Option<u64> {
    let s = s.trim_start();
    let rest = s.strip_prefix('#')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Derive the set of blocked issue numbers: an issue is blocked when it has a
/// native `blockedBy` edge to an open issue, or its body references (via
/// `blockers_in_body`) another *open* issue in this repo. Self-references and
/// references to closed/absent issues never block. Pure.
fn compute_blocked_set(issues: &[RawBlockedIssue]) -> HashSet<u64> {
    let open: HashSet<u64> = issues.iter().map(|i| i.number).collect();
    issues
        .iter()
        .filter(|i| {
            let native = i.native.iter().any(|b| b.open);
            let by_body = blockers_in_body(&i.body)
                .into_iter()
                .any(|n| n != i.number && open.contains(&n));
            native || by_body
        })
        .map(|i| i.number)
        .collect()
}

// JSON shape of `gh api graphql` output for BLOCKED_QUERY.
#[derive(Debug, Deserialize)]
struct GqlBlockerNode {
    #[serde(default)]
    state: String,
}
#[derive(Debug, Deserialize, Default)]
struct GqlConn {
    #[serde(default)]
    nodes: Vec<GqlBlockerNode>,
}
#[derive(Debug, Deserialize)]
struct GqlIssueNode {
    number: u64,
    #[serde(default)]
    body: Option<String>,
    #[serde(default, rename = "blockedBy")]
    blocked_by: GqlConn,
}
#[derive(Debug, Deserialize)]
struct GqlIssues {
    #[serde(default)]
    nodes: Vec<GqlIssueNode>,
}
#[derive(Debug, Deserialize)]
struct GqlRepository {
    issues: GqlIssues,
}
#[derive(Debug, Deserialize)]
struct GqlData {
    repository: Option<GqlRepository>,
}
#[derive(Debug, Deserialize)]
struct GqlResponse {
    #[serde(default)]
    data: Option<GqlData>,
}

/// Parse `gh api graphql` output for [`BLOCKED_QUERY`] into the set of blocked
/// issue numbers. `Err` on malformed JSON or a missing `repository` (e.g. the
/// `blockedBy` field is unavailable), so the caller can degrade to "all
/// unblocked" gracefully.
pub fn parse_blocked(json: &str) -> Result<HashSet<u64>, String> {
    let resp: GqlResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let repo = resp
        .data
        .and_then(|d| d.repository)
        .ok_or_else(|| "no repository in graphql response".to_string())?;
    let issues: Vec<RawBlockedIssue> = repo
        .issues
        .nodes
        .into_iter()
        .map(|n| RawBlockedIssue {
            number: n.number,
            body: n.body.unwrap_or_default(),
            native: n
                .blocked_by
                .nodes
                .into_iter()
                .map(|b| RawBlocker {
                    open: !b.state.eq_ignore_ascii_case("closed"),
                })
                .collect(),
        })
        .collect();
    Ok(compute_blocked_set(&issues))
}

// ── relative time ────────────────────────────────────────────────────────────

/// A compact relative-time label ("3d", "2w", "just now") for an RFC3339
/// timestamp, relative to now. Falls back to the date portion when unparseable.
pub fn relative_time(iso: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(iso) {
        Ok(dt) => relative_between(
            dt.with_timezone(&chrono::Utc),
            chrono::Utc::now(),
        ),
        Err(_) => iso.split('T').next().unwrap_or(iso).to_string(),
    }
}

/// Compact relative label for `then` measured against `now`. Pure (no clock)
/// so it is unit-testable. Future timestamps clamp to "just now".
fn relative_between(
    then: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let secs = (now - then).num_seconds();
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    if days < 30 {
        return format!("{}w", days / 7);
    }
    if days < 365 {
        return format!("{}mo", days / 30);
    }
    format!("{}y", days / 365)
}

// ── async fetching ─────────────────────────────────────────────────────────

/// Fields requested from `gh issue list`.
const LIST_FIELDS: &str = "number,title,state,labels,assignees,author,updatedAt,url";
/// Fields requested from `gh issue view`.
const DETAIL_FIELDS: &str =
    "number,title,state,stateReason,body,author,createdAt,updatedAt,labels,assignees,comments,url";

/// A completed detail fetch: which issue, and its detail or an error.
type DetailResult = (u64, Result<IssueDetail, String>);

/// Whether `start_detail` should spawn a fetch for `number`: only when it is
/// neither already cached nor the number a fetch is already in flight for. A
/// fetch for a *different* number is always started (replacing the in-flight
/// one), so opening issue #7 while #42's fetch runs never gets dropped.
fn should_start_detail(is_cached: bool, pending: Option<u64>, number: u64) -> bool {
    !is_cached && pending != Some(number)
}

/// Turn a `gh` invocation into a parsed value: parse stdout on success, else an
/// error from stderr (or a generic fallback).
fn parse_output<T>(
    out: Result<gh::Output, String>,
    parse: impl FnOnce(&str) -> Result<T, String>,
    what: &str,
) -> Result<T, String> {
    let out = out?;
    if out.success {
        parse(&out.stdout)
    } else if !out.stderr.is_empty() {
        Err(out.stderr)
    } else {
        Err(format!("{what} failed"))
    }
}

/// On-demand background fetcher for issues: the list, per-number details (cached
/// for the session), and the label set (fetched once).
#[derive(Default)]
pub struct IssueFetch {
    list_rx: Option<Receiver<Result<Vec<IssueInfo>, String>>>,
    detail_rx: Option<Receiver<DetailResult>>,
    /// Issue currently being fetched, so a dropped worker resolves to an error
    /// for the right number instead of leaving the popup stuck loading.
    pending_detail: Option<u64>,
    detail_cache: HashMap<u64, IssueDetail>,
    labels_rx: Option<Receiver<Result<Vec<IssueLabel>, String>>>,
    labels_cache: Option<Vec<IssueLabel>>,
    blocked_rx: Option<Receiver<Result<HashSet<u64>, String>>>,
    /// Set of issue numbers with an open blocker. `None` until a fetch resolves;
    /// an empty set means "nothing blocked" (distinct from "not yet known").
    blocked_cache: Option<HashSet<u64>>,
}

impl IssueFetch {
    pub fn new() -> Self {
        Self::default()
    }

    // ── list ──────────────────────────────────────────────────────────

    /// Spawn the issue-list fetch for `filter`. Replaces any in-flight one.
    pub fn start_list(&mut self, repo_path: &str, filter: IssueFilter) {
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        let state = filter.as_arg();
        thread::spawn(move || {
            let out = gh::run(
                &path,
                &[
                    "issue", "list", "--state", state, "--limit", "100", "--json", LIST_FIELDS,
                ],
                LIST_TIMEOUT,
            );
            let _ = tx.send(parse_output(out, parse_issue_list, "gh issue list"));
        });
        self.list_rx = Some(rx);
    }

    /// Poll the issue-list fetch; `Some` once on completion.
    pub fn poll_list(&mut self) -> Option<Result<Vec<IssueInfo>, String>> {
        let rx = self.list_rx.as_ref()?;
        match rx.try_recv() {
            Ok(r) => {
                self.list_rx = None;
                Some(r)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.list_rx = None;
                Some(Err("issue list worker exited".to_string()))
            }
        }
    }

    // ── detail ────────────────────────────────────────────────────────

    /// A cached detail for `number`, if fetched this session.
    pub fn cached_detail(&self, number: u64) -> Option<&IssueDetail> {
        self.detail_cache.get(&number)
    }

    /// Spawn a detail fetch for `number` unless cached or already the pending
    /// fetch. A fetch for a *different* number replaces any in-flight one (the
    /// orphaned worker's `send` fails harmlessly; `poll_detail` matches by
    /// number), so switching issues while a fetch is in flight never stalls.
    pub fn start_detail(&mut self, repo_path: &str, number: u64) {
        if !should_start_detail(self.detail_cache.contains_key(&number), self.pending_detail, number)
        {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let out = gh::run(
                &path,
                &[
                    "issue",
                    "view",
                    &number.to_string(),
                    "--json",
                    DETAIL_FIELDS,
                ],
                DETAIL_TIMEOUT,
            );
            let result = parse_output(out, parse_issue_detail, "gh issue view");
            let _ = tx.send((number, result));
        });
        self.detail_rx = Some(rx);
        self.pending_detail = Some(number);
    }

    /// Poll the detail fetch; caches success and returns `(number, result)`
    /// once. A dropped worker resolves to an error for the pending issue.
    pub fn poll_detail(&mut self) -> Option<DetailResult> {
        let rx = self.detail_rx.as_ref()?;
        let (number, result) = match rx.try_recv() {
            Ok(v) => v,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.detail_rx = None;
                let number = self.pending_detail.take().unwrap_or_default();
                return Some((number, Err("issue detail worker exited".to_string())));
            }
        };
        self.detail_rx = None;
        self.pending_detail = None;
        if let Ok(detail) = &result {
            self.detail_cache.insert(number, detail.clone());
        }
        Some((number, result))
    }

    /// Drop the cached detail for `number` so the next open refetches it (e.g.
    /// after commenting or a state change).
    pub fn invalidate_detail(&mut self, number: u64) {
        self.detail_cache.remove(&number);
    }

    // ── labels ────────────────────────────────────────────────────────

    /// The label set, if fetched this session.
    pub fn cached_labels(&self) -> Option<&Vec<IssueLabel>> {
        self.labels_cache.as_ref()
    }

    /// Spawn the label-list fetch unless cached or already in flight.
    pub fn start_labels(&mut self, repo_path: &str) {
        if self.labels_cache.is_some() || self.labels_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let out = gh::run(
                &path,
                &["label", "list", "--json", "name,color", "--limit", "100"],
                LABEL_TIMEOUT,
            );
            let _ = tx.send(parse_output(out, parse_label_list, "gh label list"));
        });
        self.labels_rx = Some(rx);
    }

    /// Poll the label-list fetch; caches success and returns it once.
    pub fn poll_labels(&mut self) -> Option<Result<Vec<IssueLabel>, String>> {
        let rx = self.labels_rx.as_ref()?;
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.labels_rx = None;
                Err("label list worker exited".to_string())
            }
        };
        self.labels_rx = None;
        if let Ok(labels) = &result {
            self.labels_cache = Some(labels.clone());
        }
        Some(result)
    }

    // ── blocked-by (dependencies) ─────────────────────────────────────

    /// The blocked-issue set, if fetched this session. Empty set = nothing
    /// blocked; `None` = not yet known (treat all as unblocked meanwhile).
    pub fn cached_blocked(&self) -> Option<&HashSet<u64>> {
        self.blocked_cache.as_ref()
    }

    /// Spawn the blocked-by fetch unless cached or already in flight. Resolves
    /// the repo's `owner/name` (needed by the GraphQL query) on the worker
    /// thread, then queries native issue dependencies + body references.
    pub fn start_blocked(&mut self, repo_path: &str) {
        if self.blocked_cache.is_some() || self.blocked_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let _ = tx.send(fetch_blocked(&path));
        });
        self.blocked_rx = Some(rx);
    }

    /// Poll the blocked-by fetch; caches success and returns it once.
    pub fn poll_blocked(&mut self) -> Option<Result<HashSet<u64>, String>> {
        let rx = self.blocked_rx.as_ref()?;
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.blocked_rx = None;
                Err("blocked-by worker exited".to_string())
            }
        };
        self.blocked_rx = None;
        if let Ok(blocked) = &result {
            self.blocked_cache = Some(blocked.clone());
        }
        Some(result)
    }

    /// Drop the cached blocked set so the next `start_blocked` refetches (e.g.
    /// after a refresh or a state change).
    pub fn invalidate_blocked(&mut self) {
        self.blocked_cache = None;
    }
}

/// Resolve `owner/name` then run the dependency GraphQL query, returning the
/// blocked-issue set. Runs on a worker thread (never the UI thread).
fn fetch_blocked(repo_path: &str) -> Result<HashSet<u64>, String> {
    let repo_out = gh::run(
        repo_path,
        &["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"],
        BLOCKED_TIMEOUT,
    )?;
    if !repo_out.success {
        return Err(if repo_out.stderr.is_empty() {
            "gh repo view failed".to_string()
        } else {
            repo_out.stderr
        });
    }
    let name_with_owner = repo_out.stdout.trim();
    let (owner, name) = name_with_owner
        .split_once('/')
        .ok_or_else(|| "could not resolve owner/name".to_string())?;
    let out = gh::run(
        repo_path,
        &[
            "api",
            "graphql",
            "-f",
            &format!("query={BLOCKED_QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
        ],
        BLOCKED_TIMEOUT,
    );
    parse_output(out, parse_blocked, "gh api graphql")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── issue list parsing ───────────────────────────────────────────

    const LIST: &str = r#"[
        {"number":42,"title":"Bug: crash on open","state":"OPEN",
         "labels":[{"name":"bug","color":"d73a4a"},{"name":"p1","color":"000000"}],
         "assignees":[{"login":"alice"},{"login":"bob"}],
         "author":{"login":"carol"},"updatedAt":"2026-07-18T08:00:00Z",
         "url":"https://github.com/o/r/issues/42"},
        {"number":7,"title":"Docs typo","state":"CLOSED",
         "labels":[],"assignees":[],"author":{"login":"dave"},
         "updatedAt":"2026-07-01T00:00:00Z","url":"https://github.com/o/r/issues/7"}
    ]"#;

    #[test]
    fn parses_issue_list_with_labels_and_assignees() {
        let issues = parse_issue_list(LIST).unwrap();
        assert_eq!(issues.len(), 2);
        let first = &issues[0];
        assert_eq!(first.number, 42);
        assert_eq!(first.title, "Bug: crash on open");
        assert_eq!(first.state, IssueState::Open);
        assert_eq!(first.author, "carol");
        assert_eq!(
            first.labels,
            vec![
                IssueLabel { name: "bug".into(), color: "d73a4a".into() },
                IssueLabel { name: "p1".into(), color: "000000".into() },
            ]
        );
        assert_eq!(first.assignees, vec!["alice", "bob"]);
        assert_eq!(first.url, "https://github.com/o/r/issues/42");
        assert_eq!(issues[1].state, IssueState::Closed);
    }

    #[test]
    fn issue_state_parses_case_insensitively() {
        // gh emits uppercase, but be tolerant of any casing.
        for (raw, want) in [
            ("OPEN", IssueState::Open),
            ("open", IssueState::Open),
            ("CLOSED", IssueState::Closed),
            ("closed", IssueState::Closed),
            ("Closed", IssueState::Closed),
        ] {
            let json = format!(r#"[{{"number":1,"state":"{raw}"}}]"#);
            assert_eq!(parse_issue_list(&json).unwrap()[0].state, want, "state {raw}");
        }
    }

    #[test]
    fn unknown_state_defaults_to_open() {
        let json = r#"[{"number":1,"state":"WEIRD"}]"#;
        assert_eq!(parse_issue_list(json).unwrap()[0].state, IssueState::Open);
    }

    #[test]
    fn empty_list_yields_empty_vec() {
        assert!(parse_issue_list("[]").unwrap().is_empty());
    }

    #[test]
    fn malformed_json_is_error() {
        assert!(parse_issue_list("not json").is_err());
        assert!(parse_issue_list("").is_err());
        assert!(parse_issue_list("{}").is_err()); // object, not an array
    }

    #[test]
    fn missing_optional_fields_degrade() {
        // Only `number` is required; everything else defaults.
        let issues = parse_issue_list(r#"[{"number":9}]"#).unwrap();
        let i = &issues[0];
        assert_eq!(i.number, 9);
        assert_eq!(i.title, "");
        assert_eq!(i.state, IssueState::Open);
        assert!(i.labels.is_empty());
        assert!(i.assignees.is_empty());
        // Missing author falls back to "ghost".
        assert_eq!(i.author, "ghost");
        assert_eq!(i.url, "");
    }

    #[test]
    fn empty_assignee_logins_are_dropped() {
        let json = r#"[{"number":1,"assignees":[{"login":""},{"login":"real"}]}]"#;
        assert_eq!(parse_issue_list(json).unwrap()[0].assignees, vec!["real"]);
    }

    #[test]
    fn unicode_title_and_author_round_trip() {
        let json = r#"[{"number":1,"title":"修复崩溃 🐛","author":{"login":"日本語"}}]"#;
        let i = &parse_issue_list(json).unwrap()[0];
        assert_eq!(i.title, "修复崩溃 🐛");
        assert_eq!(i.author, "日本語");
    }

    #[test]
    fn large_list_parses() {
        let rows: Vec<String> = (0..1000)
            .map(|n| format!(r#"{{"number":{n},"title":"issue {n}","state":"OPEN"}}"#))
            .collect();
        let json = format!("[{}]", rows.join(","));
        let issues = parse_issue_list(&json).unwrap();
        assert_eq!(issues.len(), 1000);
        assert_eq!(issues[999].number, 999);
    }

    // ── issue detail parsing ─────────────────────────────────────────

    const DETAIL: &str = r#"{
        "number":42,"title":"Bug","state":"CLOSED","stateReason":"COMPLETED",
        "body":"Steps to reproduce","author":{"login":"carol"},
        "createdAt":"2026-07-01T00:00:00Z","updatedAt":"2026-07-18T08:00:00Z",
        "labels":[{"name":"bug","color":"d73a4a"}],
        "assignees":[{"login":"alice"}],
        "comments":[
            {"author":{"login":"bob"},"createdAt":"2026-07-02T00:00:00Z","body":"confirmed"},
            {"author":{"login":"carol"},"createdAt":"2026-07-03T00:00:00Z","body":"fixed"}
        ],
        "url":"https://github.com/o/r/issues/42"
    }"#;

    #[test]
    fn parses_issue_detail_with_comments() {
        let d = parse_issue_detail(DETAIL).unwrap();
        assert_eq!(d.number, 42);
        assert_eq!(d.state, IssueState::Closed);
        assert_eq!(d.state_reason.as_deref(), Some("COMPLETED"));
        assert_eq!(d.body, "Steps to reproduce");
        assert_eq!(d.author, "carol");
        assert_eq!(d.labels.len(), 1);
        assert_eq!(d.assignees, vec!["alice"]);
        assert_eq!(d.comments.len(), 2);
        assert_eq!(d.comments[0].author, "bob");
        assert_eq!(d.comments[0].body, "confirmed");
        assert_eq!(d.comments[1].created_at, "2026-07-03T00:00:00Z");
    }

    #[test]
    fn open_issue_detail_has_no_state_reason() {
        // gh emits an empty string for an open issue; it should read as None.
        let json = r#"{"number":1,"state":"OPEN","stateReason":"","body":"x"}"#;
        let d = parse_issue_detail(json).unwrap();
        assert_eq!(d.state, IssueState::Open);
        assert_eq!(d.state_reason, None);
    }

    #[test]
    fn null_body_and_missing_optionals_degrade() {
        let json = r#"{"number":5,"body":null,"stateReason":null}"#;
        let d = parse_issue_detail(json).unwrap();
        assert_eq!(d.number, 5);
        assert_eq!(d.body, "");
        assert_eq!(d.state_reason, None);
        assert_eq!(d.author, "ghost");
        assert!(d.comments.is_empty());
        assert!(d.labels.is_empty());
    }

    #[test]
    fn detail_comment_missing_author_is_ghost() {
        let json = r#"{"number":1,"comments":[{"body":"anon","createdAt":"t"}]}"#;
        let d = parse_issue_detail(json).unwrap();
        assert_eq!(d.comments[0].author, "ghost");
        assert_eq!(d.comments[0].body, "anon");
    }

    #[test]
    fn detail_unicode_body_round_trips() {
        let json = r#"{"number":1,"body":"日本語 body 🚀","comments":[]}"#;
        assert_eq!(parse_issue_detail(json).unwrap().body, "日本語 body 🚀");
    }

    #[test]
    fn detail_malformed_json_is_error() {
        assert!(parse_issue_detail("not json").is_err());
        assert!(parse_issue_detail("").is_err());
        assert!(parse_issue_detail("[]").is_err()); // array, not an object
    }

    #[test]
    fn detail_large_body_parses() {
        let big = "x".repeat(200_000);
        let json = format!(r#"{{"number":1,"body":"{big}"}}"#);
        assert_eq!(parse_issue_detail(&json).unwrap().body.len(), 200_000);
    }

    // ── label list parsing ───────────────────────────────────────────

    #[test]
    fn parses_label_list() {
        let json = r#"[{"name":"bug","color":"d73a4a"},{"name":"enhancement","color":"a2eeef"}]"#;
        let labels = parse_label_list(json).unwrap();
        assert_eq!(
            labels,
            vec![
                IssueLabel { name: "bug".into(), color: "d73a4a".into() },
                IssueLabel { name: "enhancement".into(), color: "a2eeef".into() },
            ]
        );
    }

    #[test]
    fn label_list_empty_and_malformed() {
        assert!(parse_label_list("[]").unwrap().is_empty());
        assert!(parse_label_list("nope").is_err());
    }

    // ── filter cycling ───────────────────────────────────────────────

    // ── detail-fetch guard ───────────────────────────────────────────

    #[test]
    fn start_detail_guard_starts_second_number_while_first_in_flight() {
        // Regression (#issues MAJOR): opening #7 while #42's fetch is in flight
        // must start #7's fetch, not silently drop it (leaving the popup stuck
        // Loading forever).
        assert!(
            should_start_detail(false, Some(42), 7),
            "a fetch for a different number replaces the in-flight one"
        );
        // Same number already in flight → don't restart.
        assert!(!should_start_detail(false, Some(7), 7));
        // Already cached → never fetch, regardless of what's pending.
        assert!(!should_start_detail(true, None, 7));
        assert!(!should_start_detail(true, Some(42), 7));
        // Nothing cached, nothing pending → fetch.
        assert!(should_start_detail(false, None, 7));
    }

    #[test]
    fn filter_cycles_and_maps_to_args() {
        assert_eq!(IssueFilter::Open.next(), IssueFilter::Closed);
        assert_eq!(IssueFilter::Closed.next(), IssueFilter::All);
        assert_eq!(IssueFilter::All.next(), IssueFilter::Open);
        assert_eq!(IssueFilter::Open.as_arg(), "open");
        assert_eq!(IssueFilter::Closed.as_arg(), "closed");
        assert_eq!(IssueFilter::All.as_arg(), "all");
    }

    // ── client-side view filter ──────────────────────────────────────

    fn issue_with(number: u64, labels: &[&str]) -> IssueInfo {
        IssueInfo {
            number,
            title: format!("issue {number}"),
            state: IssueState::Open,
            labels: labels
                .iter()
                .map(|n| IssueLabel { name: (*n).to_string(), color: String::new() })
                .collect(),
            assignees: vec![],
            author: "ghost".into(),
            updated_at: String::new(),
            url: String::new(),
        }
    }

    #[test]
    fn empty_view_filter_passes_everything() {
        let f = IssueViewFilter::default();
        assert!(!f.is_active());
        let i = issue_with(1, &["bug"]);
        assert!(issue_matches(&i, &f, false));
        assert!(issue_matches(&i, &f, true)); // blocked, but no unblocked filter
    }

    #[test]
    fn label_filter_requires_at_least_one_selected_label() {
        let f = IssueViewFilter { labels: vec!["bug".into(), "p1".into()], unblocked_only: false };
        assert!(f.is_active());
        assert!(issue_matches(&issue_with(1, &["bug"]), &f, false));
        assert!(issue_matches(&issue_with(2, &["p1", "docs"]), &f, false));
        assert!(!issue_matches(&issue_with(3, &["docs"]), &f, false));
        assert!(!issue_matches(&issue_with(4, &[]), &f, false));
    }

    #[test]
    fn unblocked_only_hides_blocked_issues() {
        let f = IssueViewFilter { labels: vec![], unblocked_only: true };
        assert!(issue_matches(&issue_with(1, &[]), &f, false));
        assert!(!issue_matches(&issue_with(2, &[]), &f, true));
    }

    #[test]
    fn label_and_unblocked_filters_compose() {
        let f = IssueViewFilter { labels: vec!["bug".into()], unblocked_only: true };
        // has label + unblocked → visible
        assert!(issue_matches(&issue_with(1, &["bug"]), &f, false));
        // has label but blocked → hidden
        assert!(!issue_matches(&issue_with(2, &["bug"]), &f, true));
        // unblocked but wrong label → hidden
        assert!(!issue_matches(&issue_with(3, &["docs"]), &f, false));
    }

    #[test]
    fn visible_issues_returns_passing_indices_in_order() {
        let issues = vec![
            issue_with(10, &["bug"]),
            issue_with(20, &["docs"]),
            issue_with(30, &["bug", "p1"]),
        ];
        let f = IssueViewFilter { labels: vec!["bug".into()], unblocked_only: true };
        let mut blocked = HashSet::new();
        blocked.insert(30); // #30 blocked → dropped despite the label
        assert_eq!(visible_issues(&issues, &f, &blocked), vec![0]);
        // No filter → all indices.
        assert_eq!(
            visible_issues(&issues, &IssueViewFilter::default(), &HashSet::new()),
            vec![0, 1, 2]
        );
    }

    // ── blocked-by body parsing ──────────────────────────────────────

    #[test]
    fn blockers_in_body_matches_phrases_and_tasklists() {
        assert_eq!(blockers_in_body("Blocked by #42"), vec![42]);
        assert_eq!(blockers_in_body("this Depends On #7 to land"), vec![7]);
        assert_eq!(
            blockers_in_body("- [ ] #3\n- [x] #4\n* [ ] #5"),
            vec![3, 5]
        );
        // Case-insensitive + de-duplicated.
        assert_eq!(blockers_in_body("BLOCKED BY #9 and blocked by #9"), vec![9]);
    }

    #[test]
    fn blockers_in_body_ignores_cross_repo_and_malformed() {
        // Cross-repo owner/repo#N is not a bare #N.
        assert!(blockers_in_body("blocked by octo/other#5").is_empty());
        // No number after the phrase.
        assert!(blockers_in_body("blocked by the weather").is_empty());
        // Empty / prose without refs.
        assert!(blockers_in_body("").is_empty());
        assert!(blockers_in_body("just a normal description #notanumber").is_empty());
    }

    fn raw(number: u64, body: &str, native: &[(u64, bool)]) -> RawBlockedIssue {
        RawBlockedIssue {
            number,
            body: body.to_string(),
            native: native.iter().map(|(_, open)| RawBlocker { open: *open }).collect(),
        }
    }

    #[test]
    fn compute_blocked_set_native_open_blocker_blocks() {
        let issues = vec![
            raw(1, "", &[(2, true)]),   // blocked by open #2
            raw(2, "", &[]),
            raw(3, "", &[(2, false)]),  // blocker closed → not blocked
        ];
        let blocked = compute_blocked_set(&issues);
        assert!(blocked.contains(&1));
        assert!(!blocked.contains(&2));
        assert!(!blocked.contains(&3));
    }

    #[test]
    fn compute_blocked_set_body_refs_only_count_open_issues() {
        let issues = vec![
            raw(1, "blocked by #2", &[]), // #2 is open (present) → blocked
            raw(2, "", &[]),
            raw(3, "depends on #99", &[]), // #99 absent (closed/other) → not blocked
        ];
        let blocked = compute_blocked_set(&issues);
        assert!(blocked.contains(&1));
        assert!(!blocked.contains(&3));
    }

    #[test]
    fn compute_blocked_set_ignores_self_reference() {
        let issues = vec![raw(5, "blocked by #5", &[])];
        assert!(compute_blocked_set(&issues).is_empty());
    }

    #[test]
    fn parse_blocked_reads_graphql_and_derives_set() {
        let json = r#"{"data":{"repository":{"issues":{"nodes":[
            {"number":1,"body":"","blockedBy":{"nodes":[{"number":2,"state":"OPEN"}]}},
            {"number":2,"body":"","blockedBy":{"nodes":[]}},
            {"number":3,"body":"blocked by #2","blockedBy":{"nodes":[]}}
        ]}}}}"#;
        let blocked = parse_blocked(json).unwrap();
        assert!(blocked.contains(&1));
        assert!(blocked.contains(&3));
        assert!(!blocked.contains(&2));
    }

    #[test]
    fn parse_blocked_missing_repository_is_error() {
        // A GHES without the blockedBy field returns errors + null data.
        assert!(parse_blocked(r#"{"data":{"repository":null}}"#).is_err());
        assert!(parse_blocked("not json").is_err());
    }

    // ── relative time ────────────────────────────────────────────────

    #[test]
    fn relative_between_buckets() {
        use chrono::{Duration, Utc};
        let now = Utc::now();
        assert_eq!(relative_between(now, now), "just now");
        assert_eq!(relative_between(now - Duration::seconds(30), now), "just now");
        assert_eq!(relative_between(now - Duration::minutes(5), now), "5m");
        assert_eq!(relative_between(now - Duration::hours(3), now), "3h");
        assert_eq!(relative_between(now - Duration::days(2), now), "2d");
        assert_eq!(relative_between(now - Duration::days(10), now), "1w");
        assert_eq!(relative_between(now - Duration::days(60), now), "2mo");
        assert_eq!(relative_between(now - Duration::days(400), now), "1y");
        // Future timestamps clamp to "just now" rather than going negative.
        assert_eq!(relative_between(now + Duration::hours(1), now), "just now");
    }

    #[test]
    fn relative_time_falls_back_to_date_on_bad_input() {
        assert!(!relative_time("2026-07-18T08:00:00Z").is_empty());
        assert_eq!(relative_time("garbage"), "garbage");
        assert_eq!(relative_time("2020-01-02Tbad"), "2020-01-02");
    }
}
