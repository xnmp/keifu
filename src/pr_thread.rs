//! PR conversation thread: description, comments, reviews, and review threads,
//! fetched via `gh api graphql` (with a `gh pr view` fallback that lacks thread
//! resolution info). Lazy, background-fetched, cached per PR for the session.
//!
//! Everything above `PrThreadFetch` is pure and unit-tested.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::gh;

const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// How many of each collection we request; extras are summarized as "…N more".
const MAX_COMMENTS: usize = 50;
const MAX_REVIEWS: usize = 50;
const MAX_THREADS: usize = 50;
const MAX_THREAD_COMMENTS: usize = 30;

/// A review's disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewItemState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Other,
}

impl ReviewItemState {
    fn from_str(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "APPROVED" => Self::Approved,
            "CHANGES_REQUESTED" => Self::ChangesRequested,
            "COMMENTED" => Self::Commented,
            "DISMISSED" => Self::Dismissed,
            _ => Self::Other,
        }
    }

    /// Short label shown in the review header.
    pub fn label(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes requested",
            Self::Commented => "commented",
            Self::Dismissed => "dismissed",
            Self::Other => "reviewed",
        }
    }
}

/// A top-level conversation item (an issue comment or a review), ordered
/// chronologically by `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationItem {
    Comment {
        author: String,
        created_at: String,
        body: String,
    },
    Review {
        author: String,
        created_at: String,
        state: ReviewItemState,
        body: String,
    },
}

impl ConversationItem {
    fn created_at(&self) -> &str {
        match self {
            Self::Comment { created_at, .. } | Self::Review { created_at, .. } => created_at,
        }
    }
}

/// One comment within a review thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    pub author: String,
    pub body: String,
}

/// A review thread anchored to a file/line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    pub path: String,
    pub line: Option<u64>,
    pub resolved: bool,
    pub comments: Vec<ThreadComment>,
    /// Comments beyond the fetched cap.
    pub more_comments: usize,
}

/// A parsed PR conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrThread {
    pub title: String,
    pub author: String,
    pub created_at: String,
    pub body: String,
    pub items: Vec<ConversationItem>,
    /// Review threads, unresolved first. `None` means the source couldn't
    /// provide them (the `gh pr view` fallback) — the UI marks them unavailable.
    pub threads: Option<Vec<ReviewThread>>,
    /// Top-level comments/reviews beyond the fetched caps.
    pub more_items: usize,
    /// Review threads beyond the fetched cap.
    pub more_threads: usize,
}

impl PrThread {
    /// Count of unresolved review threads (0 when threads are unavailable).
    pub fn unresolved_count(&self) -> usize {
        self.threads
            .as_ref()
            .map_or(0, |ts| ts.iter().filter(|t| !t.resolved).count())
    }
}

// ── GraphQL parsing ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GqlActor {
    #[serde(default)]
    login: String,
}

fn actor_login(a: &Option<GqlActor>) -> String {
    a.as_ref()
        .map(|a| a.login.clone())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "ghost".to_string())
}

/// A GraphQL connection: `{ totalCount, nodes: [...] }`. Both fields are always
/// present when the connection is requested; a whole missing connection field
/// falls back to `Default` (an empty connection) via the field-level
/// `#[serde(default)]` on the parent.
#[derive(Debug, Deserialize)]
struct GqlConn<T> {
    #[serde(rename = "totalCount")]
    total_count: usize,
    nodes: Vec<T>,
}

impl<T> Default for GqlConn<T> {
    fn default() -> Self {
        Self {
            total_count: 0,
            nodes: Vec::new(),
        }
    }
}

impl<T> GqlConn<T> {
    /// Count beyond what was fetched.
    fn extra(&self) -> usize {
        self.total_count.saturating_sub(self.nodes.len())
    }
}

#[derive(Debug, Deserialize)]
struct GqlComment {
    #[serde(default)]
    author: Option<GqlActor>,
    #[serde(default)]
    body: String,
    #[serde(default, rename = "createdAt")]
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct GqlReview {
    #[serde(default)]
    author: Option<GqlActor>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: String,
    #[serde(default, rename = "createdAt")]
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct GqlThreadComment {
    #[serde(default)]
    author: Option<GqlActor>,
    #[serde(default)]
    body: String,
}

#[derive(Debug, Deserialize)]
struct GqlThread {
    #[serde(default, rename = "isResolved")]
    is_resolved: bool,
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    comments: GqlConn<GqlThreadComment>,
}

#[derive(Debug, Deserialize)]
struct GqlPr {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default, rename = "createdAt")]
    created_at: String,
    #[serde(default)]
    author: Option<GqlActor>,
    #[serde(default)]
    comments: GqlConn<GqlComment>,
    #[serde(default)]
    reviews: GqlConn<GqlReview>,
    #[serde(default, rename = "reviewThreads")]
    review_threads: GqlConn<GqlThread>,
}

#[derive(Debug, Deserialize)]
struct GqlRepo {
    #[serde(rename = "pullRequest")]
    pull_request: Option<GqlPr>,
}

#[derive(Debug, Deserialize)]
struct GqlData {
    repository: Option<GqlRepo>,
}

#[derive(Debug, Deserialize)]
struct GqlResponse {
    data: Option<GqlData>,
}

/// Keep a review as a conversation item only when it carries information: a
/// non-empty body, or a decisive state (approved / changes requested /
/// dismissed). Empty "commented" reviews are just line-comment containers.
fn review_is_meaningful(state: ReviewItemState, body: &str) -> bool {
    !body.trim().is_empty()
        || matches!(
            state,
            ReviewItemState::Approved
                | ReviewItemState::ChangesRequested
                | ReviewItemState::Dismissed
        )
}

/// Parse a `gh api graphql` response into a `PrThread`. Returns `None` if the
/// PR object is missing (so the caller can fall back).
pub fn parse_graphql(json: &str) -> Option<PrThread> {
    let resp: GqlResponse = serde_json::from_str(json).ok()?;
    let pr = resp.data?.repository?.pull_request?;

    let mut items: Vec<ConversationItem> = Vec::new();
    for c in &pr.comments.nodes {
        items.push(ConversationItem::Comment {
            author: actor_login(&c.author),
            created_at: c.created_at.clone(),
            body: c.body.clone(),
        });
    }
    for r in &pr.reviews.nodes {
        let state = ReviewItemState::from_str(&r.state);
        if !review_is_meaningful(state, &r.body) {
            continue;
        }
        items.push(ConversationItem::Review {
            author: actor_login(&r.author),
            created_at: r.created_at.clone(),
            state,
            body: r.body.clone(),
        });
    }
    sort_items(&mut items);

    let mut threads: Vec<ReviewThread> = pr
        .review_threads
        .nodes
        .iter()
        .map(|t| ReviewThread {
            path: t.path.clone(),
            line: t.line,
            resolved: t.is_resolved,
            comments: t
                .comments
                .nodes
                .iter()
                .map(|c| ThreadComment {
                    author: actor_login(&c.author),
                    body: c.body.clone(),
                })
                .collect(),
            more_comments: t.comments.extra(),
        })
        .collect();
    // Unresolved threads first (the user's priority), then resolved; original
    // order preserved within each group.
    threads.sort_by_key(|t| t.resolved);

    Some(PrThread {
        title: pr.title,
        author: actor_login(&pr.author),
        created_at: pr.created_at,
        body: pr.body,
        items,
        more_items: pr.comments.extra() + pr.reviews.extra(),
        threads: Some(threads),
        more_threads: pr.review_threads.extra(),
    })
}

/// Chronological order by RFC3339 `createdAt` (lexical sort is chronological).
fn sort_items(items: &mut [ConversationItem]) {
    items.sort_by(|a, b| a.created_at().cmp(b.created_at()));
}

// ── fallback (gh pr view) parsing ────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RestActor {
    #[serde(default)]
    login: String,
}

#[derive(Debug, Deserialize)]
struct RestComment {
    #[serde(default)]
    author: Option<RestActor>,
    #[serde(default)]
    body: String,
    #[serde(default, rename = "createdAt")]
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct RestReview {
    #[serde(default)]
    author: Option<RestActor>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: String,
    #[serde(default, rename = "submittedAt")]
    submitted_at: String,
}

#[derive(Debug, Deserialize)]
struct RestPr {
    #[serde(default)]
    title: String,
    #[serde(default)]
    author: Option<RestActor>,
    #[serde(default)]
    body: String,
    #[serde(default, rename = "createdAt")]
    created_at: String,
    #[serde(default)]
    comments: Vec<RestComment>,
    #[serde(default)]
    reviews: Vec<RestReview>,
}

fn rest_login(a: &Option<RestActor>) -> String {
    a.as_ref()
        .map(|a| a.login.clone())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "ghost".to_string())
}

/// Parse `gh pr view --json title,author,body,createdAt,comments,reviews`
/// output. Review threads are unavailable in this shape (`threads: None`).
pub fn parse_fallback(json: &str) -> Option<PrThread> {
    let pr: RestPr = serde_json::from_str(json).ok()?;
    let mut items: Vec<ConversationItem> = Vec::new();
    for c in &pr.comments {
        items.push(ConversationItem::Comment {
            author: rest_login(&c.author),
            created_at: c.created_at.clone(),
            body: c.body.clone(),
        });
    }
    for r in &pr.reviews {
        let state = ReviewItemState::from_str(&r.state);
        if !review_is_meaningful(state, &r.body) {
            continue;
        }
        items.push(ConversationItem::Review {
            author: rest_login(&r.author),
            created_at: r.submitted_at.clone(),
            state,
            body: r.body.clone(),
        });
    }
    sort_items(&mut items);
    Some(PrThread {
        title: pr.title,
        author: rest_login(&pr.author),
        created_at: pr.created_at,
        body: pr.body,
        items,
        more_items: 0,
        threads: None,
        more_threads: 0,
    })
}

// ── body preprocessing (very light markdown normalization) ───────────────

/// Normalize a comment/PR body for plain rendering: split into lines and
/// collapse runs of 3+ blank lines to a single blank. We deliberately do NOT
/// parse inline markdown (bold/italic/links/`code`) — text renders as-is; the
/// widget only dims fenced code blocks.
pub fn preprocess_body(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut blank_run = 0usize;
    for line in body.replace('\r', "").lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push(String::new());
            }
        } else {
            blank_run = 0;
            out.push(line.to_string());
        }
    }
    // Trim leading/trailing blank lines.
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

// ── async fetching ───────────────────────────────────────────────────────

fn fetch_graphql(repo_path: &str, pr_number: u64) -> Result<PrThread, String> {
    let query = format!(
        "query($owner:String!,$repo:String!,$number:Int!){{\
           repository(owner:$owner,name:$repo){{\
             pullRequest(number:$number){{\
               title body createdAt author{{login}}\
               comments(first:{MAX_COMMENTS}){{totalCount nodes{{author{{login}} body createdAt}}}}\
               reviews(first:{MAX_REVIEWS}){{totalCount nodes{{author{{login}} state body createdAt}}}}\
               reviewThreads(first:{MAX_THREADS}){{totalCount nodes{{isResolved path line \
                 comments(first:{MAX_THREAD_COMMENTS}){{totalCount nodes{{author{{login}} body}}}}}}}}\
             }}}}}}"
    );
    let out = gh::run(
        repo_path,
        &[
            "api",
            "graphql",
            "-F",
            "owner={owner}",
            "-F",
            "repo={repo}",
            "-F",
            &format!("number={pr_number}"),
            "-f",
            &format!("query={query}"),
        ],
        FETCH_TIMEOUT,
    )?;
    if !out.success && out.stdout.trim().is_empty() {
        return Err(if out.stderr.is_empty() {
            "graphql query failed".to_string()
        } else {
            out.stderr
        });
    }
    parse_graphql(&out.stdout).ok_or_else(|| "unexpected graphql response".to_string())
}

fn fetch_fallback(repo_path: &str, pr_number: u64) -> Result<PrThread, String> {
    let out = gh::run(
        repo_path,
        &[
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "title,author,body,createdAt,comments,reviews",
        ],
        FETCH_TIMEOUT,
    )?;
    if !out.success && out.stdout.trim().is_empty() {
        return Err(if out.stderr.is_empty() {
            "gh pr view failed".to_string()
        } else {
            out.stderr
        });
    }
    parse_fallback(&out.stdout).ok_or_else(|| "unexpected pr view response".to_string())
}

/// Background fetcher for one PR's conversation, cached per PR number.
#[derive(Default)]
pub struct PrThreadFetch {
    rx: Option<Receiver<(u64, Result<PrThread, String>)>>,
    cache: HashMap<u64, PrThread>,
}

impl PrThreadFetch {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cached conversation for `pr_number`, if fetched this session.
    pub fn cached(&self, pr_number: u64) -> Option<&PrThread> {
        self.cache.get(&pr_number)
    }

    /// Spawn a fetch for `pr_number` unless cached or already in flight. The
    /// worker tries GraphQL, then falls back to `gh pr view`.
    pub fn start(&mut self, repo_path: &str, pr_number: u64) {
        if self.cache.contains_key(&pr_number) || self.rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let result = fetch_graphql(&path, pr_number)
                .or_else(|_| fetch_fallback(&path, pr_number));
            let _ = tx.send((pr_number, result));
        });
        self.rx = Some(rx);
    }

    /// Poll the fetch; caches success and returns `(pr_number, result)` once.
    pub fn poll(&mut self) -> Option<(u64, Result<PrThread, String>)> {
        let rx = self.rx.as_ref()?;
        let (pr_number, result) = match rx.try_recv() {
            Ok(v) => v,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                return None;
            }
        };
        self.rx = None;
        if let Ok(thread) = &result {
            self.cache.insert(pr_number, thread.clone());
        }
        Some((pr_number, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── GraphQL parsing ──────────────────────────────────────────────

    const FULL: &str = r#"{"data":{"repository":{"pullRequest":{
        "title":"Add feature","body":"Body line 1\n\n\n\nBody line 2","createdAt":"2026-07-01T00:00:00Z",
        "author":{"login":"alice"},
        "comments":{"totalCount":2,"nodes":[
            {"author":{"login":"bob"},"body":"looks good","createdAt":"2026-07-03T00:00:00Z"}
        ]},
        "reviews":{"totalCount":3,"nodes":[
            {"author":{"login":"carol"},"state":"CHANGES_REQUESTED","body":"please fix","createdAt":"2026-07-02T00:00:00Z"},
            {"author":{"login":"dave"},"state":"APPROVED","body":"","createdAt":"2026-07-04T00:00:00Z"},
            {"author":{"login":"eve"},"state":"COMMENTED","body":"","createdAt":"2026-07-05T00:00:00Z"}
        ]},
        "reviewThreads":{"totalCount":2,"nodes":[
            {"isResolved":true,"path":"src/a.rs","line":10,"comments":{"totalCount":1,"nodes":[{"author":{"login":"carol"},"body":"nit"}]}},
            {"isResolved":false,"path":"src/b.rs","line":null,"comments":{"totalCount":5,"nodes":[
                {"author":{"login":"carol"},"body":"this is wrong"},{"author":{"login":"alice"},"body":"fixed?"}
            ]}}
        ]}
    }}}}"#;

    #[test]
    fn graphql_full_shape_parses() {
        let t = parse_graphql(FULL).unwrap();
        assert_eq!(t.title, "Add feature");
        assert_eq!(t.author, "alice");
        // Empty COMMENTED review is dropped; changes-requested + approved kept +
        // the one comment = 3 items.
        assert_eq!(t.items.len(), 3);
        assert!(matches!(
            t.items[0],
            ConversationItem::Review {
                state: ReviewItemState::ChangesRequested,
                ..
            }
        ));
    }

    #[test]
    fn items_are_chronological() {
        let t = parse_graphql(FULL).unwrap();
        let times: Vec<&str> = t.items.iter().map(|i| i.created_at()).collect();
        // carol review (07-02) < bob comment (07-03) < dave approved (07-04).
        assert_eq!(
            times,
            vec![
                "2026-07-02T00:00:00Z",
                "2026-07-03T00:00:00Z",
                "2026-07-04T00:00:00Z"
            ]
        );
    }

    #[test]
    fn threads_unresolved_first_and_classified() {
        let t = parse_graphql(FULL).unwrap();
        let threads = t.threads.as_ref().unwrap();
        assert_eq!(threads.len(), 2);
        // Unresolved (src/b.rs) sorts before resolved (src/a.rs).
        assert_eq!(threads[0].path, "src/b.rs");
        assert!(!threads[0].resolved);
        assert_eq!(threads[0].line, None);
        assert_eq!(threads[1].path, "src/a.rs");
        assert!(threads[1].resolved);
        assert_eq!(t.unresolved_count(), 1);
    }

    #[test]
    fn truncation_counts_extras() {
        let t = parse_graphql(FULL).unwrap();
        // comments totalCount 2 shown 1 (=1) + reviews total 3 shown 3 (=0) => 1.
        assert_eq!(t.more_items, 1);
        // The unresolved thread reports 5 total, 2 shown => 3 more.
        let unresolved = &t.threads.as_ref().unwrap()[0];
        assert_eq!(unresolved.more_comments, 3);
    }

    #[test]
    fn graphql_missing_or_null_fields_degrade() {
        // Null author, missing collections, null PR.
        let partial = r#"{"data":{"repository":{"pullRequest":{
            "title":"T","body":"","createdAt":"","author":null
        }}}}"#;
        let t = parse_graphql(partial).unwrap();
        assert_eq!(t.author, "ghost");
        assert!(t.items.is_empty());
        assert_eq!(t.threads.as_ref().unwrap().len(), 0);

        // Missing PR object → None (caller falls back).
        assert!(parse_graphql(r#"{"data":{"repository":{"pullRequest":null}}}"#).is_none());
        assert!(parse_graphql("not json").is_none());
    }

    // ── fallback parsing ─────────────────────────────────────────────

    #[test]
    fn fallback_shape_parses_without_threads() {
        let json = r#"{
            "title":"T","author":{"login":"alice"},"body":"desc","createdAt":"2026-07-01T00:00:00Z",
            "comments":[{"author":{"login":"bob"},"body":"hi","createdAt":"2026-07-02T00:00:00Z"}],
            "reviews":[{"author":{"login":"carol"},"state":"APPROVED","body":"lgtm","submittedAt":"2026-07-03T00:00:00Z"}]
        }"#;
        let t = parse_fallback(json).unwrap();
        assert_eq!(t.author, "alice");
        assert_eq!(t.items.len(), 2);
        assert!(t.threads.is_none(), "threads unavailable in fallback");
        assert_eq!(t.unresolved_count(), 0);
    }

    // ── body preprocessing ───────────────────────────────────────────

    #[test]
    fn preprocess_collapses_blank_runs_and_trims() {
        let out = preprocess_body("\n\nline1\n\n\n\nline2\n\n");
        assert_eq!(out, vec!["line1", "", "line2"]);
    }

    #[test]
    fn preprocess_empty_body() {
        assert!(preprocess_body("").is_empty());
        assert!(preprocess_body("\n\n  \n").is_empty());
    }

    // ── review state mapping ─────────────────────────────────────────

    #[test]
    fn review_state_mapping_and_meaningfulness() {
        assert_eq!(ReviewItemState::from_str("APPROVED"), ReviewItemState::Approved);
        assert_eq!(
            ReviewItemState::from_str("changes_requested"),
            ReviewItemState::ChangesRequested
        );
        assert_eq!(ReviewItemState::from_str("WEIRD"), ReviewItemState::Other);
        // Empty commented review is noise; approved-empty is meaningful.
        assert!(!review_is_meaningful(ReviewItemState::Commented, ""));
        assert!(review_is_meaningful(ReviewItemState::Approved, ""));
        assert!(review_is_meaningful(ReviewItemState::Commented, "nice"));
    }
}
