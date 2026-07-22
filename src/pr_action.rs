//! Mutating PR actions via the `gh` CLI: create, merge, review.
//!
//! Command construction is pure (unit-tested). Execution runs on a background
//! thread through the shared `gh` runner; bodies are passed via `--body-file`
//! (a temp file) to sidestep arg-length and shell-quoting issues. All time and
//! file paths are injectable so nothing here needs a real shell to test.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::gh;
use crate::pr::PrInfo;

const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// How to integrate a merged PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    fn flag(self) -> &'static str {
        match self {
            Self::Merge => "--merge",
            Self::Squash => "--squash",
            Self::Rebase => "--rebase",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "Merge commit",
            Self::Squash => "Squash and merge",
            Self::Rebase => "Rebase and merge",
        }
    }

    pub const ALL: [MergeMethod; 3] = [Self::Merge, Self::Squash, Self::Rebase];
}

/// A review disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewDecision {
    fn flag(self) -> &'static str {
        match self {
            Self::Approve => "--approve",
            Self::RequestChanges => "--request-changes",
            Self::Comment => "--comment",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "Approve",
            Self::RequestChanges => "Request changes",
            Self::Comment => "Comment",
        }
    }

    /// Whether this decision requires a body (and thus a `--body-file`).
    pub fn needs_body(self) -> bool {
        matches!(self, Self::RequestChanges | Self::Comment)
    }

    pub const ALL: [ReviewDecision; 3] = [Self::Approve, Self::RequestChanges, Self::Comment];
}

/// A pending mutating PR action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrAction {
    /// Create a PR from the current branch. `body` may be empty; the base is
    /// left to gh (the repo's default branch).
    Create { title: String, body: String },
    Merge { number: u64, method: MergeMethod },
    Review {
        number: u64,
        decision: ReviewDecision,
        body: String,
    },
}

impl PrAction {
    /// The body text to write to a `--body-file`, or `None` when the action
    /// takes no body (merge, or a bare approve).
    pub fn body(&self) -> Option<&str> {
        match self {
            Self::Create { body, .. } => Some(body),
            Self::Review { decision, body, .. } if decision.needs_body() => Some(body),
            _ => None,
        }
    }

    /// The gh argument vector. `body_file` is the path passed to `--body-file`
    /// for actions that take a body (see [`Self::body`]); ignored otherwise.
    pub fn build_args(&self, body_file: Option<&str>) -> Vec<String> {
        let s = |x: &str| x.to_string();
        match self {
            Self::Create { title, .. } => {
                let mut args = vec![s("pr"), s("create"), s("--title"), title.clone()];
                if let Some(path) = body_file {
                    args.push(s("--body-file"));
                    args.push(s(path));
                }
                args
            }
            Self::Merge { number, method } => vec![
                s("pr"),
                s("merge"),
                number.to_string(),
                s(method.flag()),
            ],
            Self::Review {
                number, decision, ..
            } => {
                let mut args =
                    vec![s("pr"), s("review"), number.to_string(), s(decision.flag())];
                if decision.needs_body() {
                    if let Some(path) = body_file {
                        args.push(s("--body-file"));
                        args.push(s(path));
                    }
                }
                args
            }
        }
    }

    /// Whether success should refresh the graph (merge can delete the branch
    /// remotely / change history on next fetch).
    pub fn refreshes_graph(&self) -> bool {
        matches!(self, Self::Merge { .. })
    }
}

/// Parse the PR number out of a `gh pr create` URL (`…/pull/<n>`).
pub fn parse_created_pr_number(stdout: &str) -> Option<u64> {
    stdout
        .split("/pull/")
        .nth(1)?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

// ── eligibility predicates (pure) ────────────────────────────────────────

/// Whether a "create PR" action should be offered: the branch is publishable
/// (has a remote to push to) and no open PR already exists for it.
pub fn can_create_pr(
    open_prs: &HashMap<String, PrInfo>,
    branch: &str,
    publishable: bool,
) -> bool {
    publishable && !branch.is_empty() && !open_prs.contains_key(branch)
}

// ── async execution ──────────────────────────────────────────────────────

/// Outcome of a completed PR action: the action and gh's result (success text
/// or error message).
pub type ActionOutcome = (PrAction, Result<String, String>);

/// Background runner for one PR action at a time.
#[derive(Default)]
pub struct PrActionRunner {
    rx: Option<Receiver<ActionOutcome>>,
}

impl PrActionRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Spawn `action` in the background. A body (if any) is written to a temp
    /// file and passed via `--body-file`, then removed. No-op if one is already
    /// in flight.
    pub fn start(&mut self, repo_path: &str, action: PrAction) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let result = run_action(&path, &action);
            let _ = tx.send((action, result));
        });
        self.rx = Some(rx);
    }

    /// Poll for completion; returns the outcome once.
    pub fn poll(&mut self) -> Option<ActionOutcome> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok(v) => {
                self.rx = None;
                Some(v)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                Some((
                    PrAction::Merge {
                        number: 0,
                        method: MergeMethod::Merge,
                    },
                    Err("PR action worker exited".to_string()),
                ))
            }
        }
    }
}

/// Temp path for the `--body-file`. Actions are serialized (one at a time), so
/// a single fixed path is safe.
fn body_file_path() -> PathBuf {
    std::env::temp_dir().join("keifu").join("pr-body.md")
}

fn run_action(repo_path: &str, action: &PrAction) -> Result<String, String> {
    // Write the body to a temp file when the action carries one.
    let body_path = match action.body() {
        Some(body) => {
            let path = body_file_path();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, body).map_err(|e| e.to_string())?;
            Some(path)
        }
        None => None,
    };
    let body_str = body_path.as_ref().and_then(|p| p.to_str());
    let args = action.build_args(body_str);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let out = gh::run(repo_path, &arg_refs, ACTION_TIMEOUT);
    if let Some(path) = &body_path {
        let _ = std::fs::remove_file(path);
    }
    let out = out?;
    if out.success {
        Ok(out.stdout)
    } else if !out.stderr.is_empty() {
        Err(out.stderr)
    } else {
        Err("gh action failed".to_string())
    }
}

/// Toast text for a successful action, using gh's output when useful.
pub fn success_message(action: &PrAction, stdout: &str) -> String {
    match action {
        PrAction::Create { .. } => match parse_created_pr_number(stdout) {
            Some(n) => format!("Created PR #{n}"),
            None => "Created PR".to_string(),
        },
        PrAction::Merge { number, method } => {
            format!("{} PR #{number}", method.label())
        }
        PrAction::Review {
            number, decision, ..
        } => format!("{} PR #{number}", decision.verb_past()),
    }
}

impl ReviewDecision {
    fn verb_past(self) -> &'static str {
        match self {
            Self::Approve => "Approved",
            Self::RequestChanges => "Requested changes on",
            Self::Comment => "Commented on",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::{CiStatus, ReviewState};

    // ── command construction ─────────────────────────────────────────

    #[test]
    fn create_args_include_title_and_body_file() {
        let a = PrAction::Create {
            title: "Add feature X".to_string(),
            body: "details".to_string(),
        };
        assert_eq!(a.body(), Some("details"));
        assert_eq!(
            a.build_args(Some("/tmp/keifu/pr-body.md")),
            vec![
                "pr",
                "create",
                "--title",
                "Add feature X",
                "--body-file",
                "/tmp/keifu/pr-body.md"
            ]
        );
        // Titles with shell-special chars go through untouched (never a shell).
        let tricky = PrAction::Create {
            title: "fix: \"quotes\" & $VARS".to_string(),
            body: String::new(),
        };
        assert_eq!(tricky.build_args(Some("/p"))[3], "fix: \"quotes\" & $VARS");
    }

    #[test]
    fn merge_args_map_method_to_flag() {
        let mk = |m| PrAction::Merge { number: 42, method: m };
        assert_eq!(
            mk(MergeMethod::Merge).build_args(None),
            vec!["pr", "merge", "42", "--merge"]
        );
        assert_eq!(mk(MergeMethod::Squash).build_args(None)[3], "--squash");
        assert_eq!(mk(MergeMethod::Rebase).build_args(None)[3], "--rebase");
        assert_eq!(mk(MergeMethod::Merge).body(), None);
    }

    #[test]
    fn review_args_and_body_by_decision() {
        let approve = PrAction::Review {
            number: 7,
            decision: ReviewDecision::Approve,
            body: String::new(),
        };
        // Approve takes no body-file even if a path is offered.
        assert_eq!(approve.body(), None);
        assert_eq!(approve.build_args(Some("/p")), vec!["pr", "review", "7", "--approve"]);

        let changes = PrAction::Review {
            number: 7,
            decision: ReviewDecision::RequestChanges,
            body: "please fix".to_string(),
        };
        assert_eq!(changes.body(), Some("please fix"));
        assert_eq!(
            changes.build_args(Some("/p")),
            vec!["pr", "review", "7", "--request-changes", "--body-file", "/p"]
        );

        let comment = PrAction::Review {
            number: 7,
            decision: ReviewDecision::Comment,
            body: "note".to_string(),
        };
        assert_eq!(comment.build_args(Some("/p"))[3], "--comment");
    }

    #[test]
    fn created_pr_number_parsing() {
        assert_eq!(
            parse_created_pr_number("https://github.com/o/r/pull/123\n"),
            Some(123)
        );
        assert_eq!(parse_created_pr_number("no url here"), None);
    }

    #[test]
    fn refreshes_graph_only_for_merge() {
        assert!(PrAction::Merge {
            number: 1,
            method: MergeMethod::Squash
        }
        .refreshes_graph());
        assert!(!PrAction::Create {
            title: "t".into(),
            body: String::new()
        }
        .refreshes_graph());
    }

    // ── eligibility ──────────────────────────────────────────────────

    fn pr(number: u64) -> PrInfo {
        PrInfo {
            number,
            url: String::new(),
            title: String::new(),
            ci: CiStatus::None,
            review: ReviewState::None,
            merge_state: crate::pr::MergeState::Clear,
            outside_activity: false,
            head_oid: None,
        }
    }

    #[test]
    fn can_create_requires_publishable_and_no_existing_pr() {
        let mut prs = HashMap::new();
        assert!(can_create_pr(&prs, "feature", true));
        // Not publishable → no.
        assert!(!can_create_pr(&prs, "feature", false));
        // Empty branch (e.g. detached HEAD) → no.
        assert!(!can_create_pr(&prs, "", true));
        // Existing open PR for the branch → no.
        prs.insert("feature".to_string(), pr(9));
        assert!(!can_create_pr(&prs, "feature", true));
    }
}
