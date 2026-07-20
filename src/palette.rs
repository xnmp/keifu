//! Command palette: a fuzzy finder over three source kinds — a curated command
//! registry, branches, and loaded commits — ranked into one list.
//!
//! This module holds the pure pieces: the command registry (with per-command
//! eligibility over a lightweight [`PaletteContext`]), the candidate/result
//! types, and the ranking function. The `App` builds the live candidate set and
//! executes the chosen item; the widget renders the results.

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::action::Action;

/// Maximum results rendered; the rest are summarised as "…N more".
pub const PALETTE_CAP: usize = 15;

/// Which source a palette row came from — also the display tag and the
/// equal-score tiebreak order (commands rank above branches above commits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKind {
    Command,
    Branch,
    Commit,
}

impl PaletteKind {
    /// The dim category tag shown per row.
    pub fn tag(self) -> &'static str {
        match self {
            PaletteKind::Command => "cmd",
            PaletteKind::Branch => "branch",
            PaletteKind::Commit => "commit",
        }
    }

    /// Sort rank at equal fuzzy score (lower wins): command < branch < commit.
    fn rank(self) -> u8 {
        match self {
            PaletteKind::Command => 0,
            PaletteKind::Branch => 1,
            PaletteKind::Commit => 2,
        }
    }
}

/// What executing a palette row does.
#[derive(Debug, Clone)]
pub enum PaletteAction {
    /// Dispatch an app action (focused on the graph panel).
    Dispatch(Action),
    /// Open the checkout confirmation for a branch. `is_remote` carries the
    /// branch's authoritative remote/local status from its `BranchInfo`.
    Checkout { name: String, is_remote: bool },
    /// Jump the graph selection to a commit by its full node index.
    JumpToCommit(usize),
}

/// Facts about the current selection that gate which commands are offered.
/// A plain value object so the registry's eligibility is pure and testable.
#[derive(Debug, Clone, Copy, Default)]
pub struct PaletteContext {
    /// A real commit (not the uncommitted/connector row) is selected.
    pub has_selected_commit: bool,
    /// The current branch has no open PR and one can be created.
    pub can_create_pr: bool,
    /// The selected commit's branch has an open PR.
    pub selected_has_open_pr: bool,
    /// More commits remain to load (the walk isn't exhausted).
    pub can_load_more: bool,
    /// The undo ledger has at least one reversible operation.
    pub can_undo: bool,
}

/// One curated command: a label, an optional right-aligned keybind hint, the
/// action to dispatch, and an eligibility predicate over the context.
#[derive(Clone)]
pub struct PaletteEntry {
    pub label: &'static str,
    pub hint: Option<&'static str>,
    pub action: Action,
    pub eligible: fn(&PaletteContext) -> bool,
}

/// The curated registry, in display order (used verbatim for the empty query).
/// Explicitly authored — NOT derived from `Action`, whose variants are mostly
/// internal/editor plumbing.
pub fn command_registry() -> Vec<PaletteEntry> {
    fn always(_: &PaletteContext) -> bool {
        true
    }
    fn has_commit(c: &PaletteContext) -> bool {
        c.has_selected_commit
    }
    fn can_create_pr(c: &PaletteContext) -> bool {
        c.can_create_pr
    }
    fn has_open_pr(c: &PaletteContext) -> bool {
        c.selected_has_open_pr
    }
    fn can_load_more(c: &PaletteContext) -> bool {
        c.can_load_more
    }
    fn can_undo(c: &PaletteContext) -> bool {
        c.can_undo
    }

    vec![
        entry("Fetch all remotes & refresh", Some("F5"), Action::FullUpdate, always),
        entry("Fetch", Some("f"), Action::Fetch, always),
        entry("Pull", Some("p"), Action::Pull, always),
        entry("Push current branch", Some("P"), Action::Push, always),
        entry("Refresh", Some("R"), Action::Refresh, always),
        entry("Commit actions menu", Some("Enter"), Action::OpenCommitMenu, has_commit),
        entry("Create branch here", Some("b"), Action::CreateBranch, has_commit),
        entry("Mark commit for compare", Some("m"), Action::MarkForCompare, has_commit),
        entry("Jump to merge base with main", Some("^"), Action::JumpToMergeBase, has_commit),
        entry("Undo last operation", Some("^Z"), Action::UndoLastOp, can_undo),
        entry("Create pull request", None, Action::CreatePullRequest, can_create_pr),
        entry("Merge pull request", None, Action::MergePullRequest, has_open_pr),
        entry("Open PR in browser", Some("o"), Action::OpenPr, has_open_pr),
        entry("View CI checks", Some("c"), Action::OpenCiChecks, has_open_pr),
        entry("View PR conversation", Some("v"), Action::OpenPrThread, has_open_pr),
        entry("Issues: list", Some("I"), Action::OpenIssueList, always),
        entry("Issues: new issue", None, Action::NewIssue, always),
        entry("Toggle branch tracing", Some("t"), Action::ToggleTrace, always),
        entry("Display columns menu", Some("M"), Action::OpenMetadataMenu, always),
        entry("Filter branches", Some("B"), Action::OpenBranchFilter, always),
        entry("Search branches", Some("/"), Action::Search, always),
        entry("Load 500 more commits", None, Action::LoadMoreCommits, can_load_more),
        entry("Load all commits", None, Action::LoadAllCommits, can_load_more),
    ]
}

fn entry(
    label: &'static str,
    hint: Option<&'static str>,
    action: Action,
    eligible: fn(&PaletteContext) -> bool,
) -> PaletteEntry {
    PaletteEntry {
        label,
        hint,
        action,
        eligible,
    }
}

/// A single rankable row from any source.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub kind: PaletteKind,
    /// Display text (left).
    pub label: String,
    /// Optional dim right-aligned hint (keybind for commands, short hash for commits).
    pub hint: Option<String>,
    /// Text the query fuzzy-matches against.
    pub match_text: String,
    /// What Enter does.
    pub action: PaletteAction,
    /// Stable within-kind tiebreak (registry index / commit row).
    pub order: usize,
}

/// The ranked, capped result set plus how many matches were hidden by the cap.
#[derive(Debug)]
pub struct PaletteResults {
    pub items: Vec<Candidate>,
    pub more: usize,
}

/// Rank `candidates` against `query`, capped to `cap`.
///
/// Empty query: commands only, in registry order (no fuzzy). Otherwise fuzzy
/// score each candidate's `match_text`, keep the matches, and sort by score
/// (desc) with a fully deterministic tiebreak: kind (command < branch <
/// commit), then `order` (asc), then label (asc).
pub fn rank(query: &str, candidates: Vec<Candidate>, cap: usize) -> PaletteResults {
    if query.trim().is_empty() {
        let mut items: Vec<Candidate> = candidates
            .into_iter()
            .filter(|c| c.kind == PaletteKind::Command)
            .collect();
        items.sort_by(|a, b| a.order.cmp(&b.order));
        let more = items.len().saturating_sub(cap);
        items.truncate(cap);
        return PaletteResults { items, more };
    }

    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, Candidate)> = candidates
        .into_iter()
        .filter_map(|c| {
            matcher
                .fuzzy_match(&c.match_text, query)
                .map(|score| (score, c))
        })
        .collect();

    scored.sort_by(|(sa, a), (sb, b)| {
        sb.cmp(sa) // higher score first
            .then_with(|| a.kind.rank().cmp(&b.kind.rank()))
            .then_with(|| a.order.cmp(&b.order))
            .then_with(|| a.label.cmp(&b.label))
    });

    let total = scored.len();
    let more = total.saturating_sub(cap);
    let items = scored.into_iter().take(cap).map(|(_, c)| c).collect();
    PaletteResults { items, more }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(kind: PaletteKind, label: &str, order: usize) -> Candidate {
        Candidate {
            kind,
            label: label.to_string(),
            hint: None,
            match_text: label.to_string(),
            action: PaletteAction::JumpToCommit(order),
            order,
        }
    }

    // ── registry eligibility ──────────────────────────────────────────

    #[test]
    fn registry_hides_ineligible_commands() {
        let reg = command_registry();
        let none = PaletteContext::default();
        // With nothing selected and no PR, PR/commit commands are hidden.
        let labels: Vec<&str> = reg
            .iter()
            .filter(|e| (e.eligible)(&none))
            .map(|e| e.label)
            .collect();
        assert!(labels.contains(&"Fetch")); // always-eligible
        assert!(!labels.contains(&"Create pull request"));
        assert!(!labels.contains(&"Merge pull request"));
        assert!(!labels.contains(&"Commit actions menu"));
        // The merge-base jump needs a selected commit.
        assert!(!labels.contains(&"Jump to merge base with main"));
        // Load-commits entries are hidden once everything is loaded.
        assert!(!labels.contains(&"Load 500 more commits"));
        assert!(!labels.contains(&"Load all commits"));

        // With more history to load, both appear.
        let more = PaletteContext {
            can_load_more: true,
            ..Default::default()
        };
        let labels: Vec<&str> = reg
            .iter()
            .filter(|e| (e.eligible)(&more))
            .map(|e| e.label)
            .collect();
        assert!(labels.contains(&"Load 500 more commits"));
        assert!(labels.contains(&"Load all commits"));

        // A commit with an open PR unlocks the PR commands.
        let ctx = PaletteContext {
            has_selected_commit: true,
            can_create_pr: false,
            selected_has_open_pr: true,
            ..Default::default()
        };
        let labels: Vec<&str> = reg
            .iter()
            .filter(|e| (e.eligible)(&ctx))
            .map(|e| e.label)
            .collect();
        assert!(labels.contains(&"Merge pull request"));
        assert!(labels.contains(&"Open PR in browser"));
        assert!(labels.contains(&"Commit actions menu"));
        assert!(labels.contains(&"Jump to merge base with main"));
        assert!(!labels.contains(&"Create pull request")); // gated separately
    }

    #[test]
    fn registry_dispatch_mapping_is_correct() {
        let reg = command_registry();
        let by_label = |l: &str| reg.iter().find(|e| e.label == l).unwrap().action.clone();
        assert!(matches!(by_label("Fetch"), Action::Fetch));
        assert!(matches!(
            by_label("Fetch all remotes & refresh"),
            Action::FullUpdate
        ));
        assert!(matches!(by_label("Toggle branch tracing"), Action::ToggleTrace));
        assert!(matches!(
            by_label("Create pull request"),
            Action::CreatePullRequest
        ));
    }

    // ── ranking / tiebreak ────────────────────────────────────────────

    #[test]
    fn equal_scores_break_by_kind_then_order() {
        // Identical match_text → identical fuzzy score for the query, so only
        // the tiebreak decides ordering.
        let candidates = vec![
            cand(PaletteKind::Commit, "foo", 0),
            cand(PaletteKind::Branch, "foo", 0),
            cand(PaletteKind::Command, "foo", 1),
            cand(PaletteKind::Command, "foo", 0),
        ];
        let res = rank("foo", candidates, 15);
        let kinds: Vec<(PaletteKind, usize)> =
            res.items.iter().map(|c| (c.kind, c.order)).collect();
        assert_eq!(
            kinds,
            vec![
                (PaletteKind::Command, 0),
                (PaletteKind::Command, 1),
                (PaletteKind::Branch, 0),
                (PaletteKind::Commit, 0),
            ]
        );
    }

    #[test]
    fn empty_query_shows_only_commands_in_registry_order() {
        let candidates = vec![
            cand(PaletteKind::Branch, "main", 0),
            cand(PaletteKind::Command, "b-second", 1),
            cand(PaletteKind::Command, "a-first", 0),
            cand(PaletteKind::Commit, "fix", 0),
        ];
        let res = rank("", candidates, 15);
        let labels: Vec<&str> = res.items.iter().map(|c| c.label.as_str()).collect();
        // Only commands, ordered by `order` (registry order), not alphabetical.
        assert_eq!(labels, vec!["a-first", "b-second"]);
        assert_eq!(res.more, 0);
    }

    #[test]
    fn cap_limits_items_and_counts_the_rest() {
        let candidates: Vec<Candidate> = (0..20)
            .map(|i| cand(PaletteKind::Command, "match", i))
            .collect();
        // Empty query path.
        let res = rank("", candidates, 15);
        assert_eq!(res.items.len(), 15);
        assert_eq!(res.more, 5);

        // Fuzzy path.
        let candidates: Vec<Candidate> = (0..20)
            .map(|i| cand(PaletteKind::Command, "match", i))
            .collect();
        let res = rank("match", candidates, 15);
        assert_eq!(res.items.len(), 15);
        assert_eq!(res.more, 5);
    }

    #[test]
    fn commit_matches_on_hash_prefix_and_subject_substring() {
        let make = |text: &str| Candidate {
            kind: PaletteKind::Commit,
            label: "Fix the bug".to_string(),
            hint: Some("abc1234".to_string()),
            match_text: text.to_string(),
            action: PaletteAction::JumpToCommit(3),
            order: 3,
        };
        // match_text = subject + short hash.
        let text = "Fix the bug abc1234";
        assert_eq!(rank("abc12", vec![make(text)], 15).items.len(), 1); // hash prefix
        assert_eq!(rank("bug", vec![make(text)], 15).items.len(), 1); // subject substr
        assert_eq!(rank("zzzz", vec![make(text)], 15).items.len(), 0); // no match
    }
}
