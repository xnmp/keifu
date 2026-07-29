//! Mutating issue actions via the `gh` CLI: create, comment, close/reopen, and
//! edit labels/assignees.
//!
//! Command construction is pure (unit-tested). Execution runs on a background
//! thread through the shared `gh` runner; bodies are passed via `--body-file`
//! (a temp file) to sidestep arg-length and shell-quoting issues — the same
//! pattern as `pr_action`.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::gh;

const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACHMENT_TIMEOUT: Duration = Duration::from_secs(90);

/// A pending mutating issue action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueAction {
    /// Create a new issue. `body` may be empty.
    Create {
        title: String,
        body: String,
        /// Short-lived clipboard image captured by the compose UI.
        attachment: Option<PathBuf>,
    },
    /// Add a comment to an existing issue.
    Comment {
        number: u64,
        body: String,
    },
    Close {
        number: u64,
    },
    Reopen {
        number: u64,
    },
    /// Add and/or remove labels in one edit.
    EditLabels {
        number: u64,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// Add and/or remove assignees in one edit.
    EditAssignees {
        number: u64,
        add: Vec<String>,
        remove: Vec<String>,
    },
}

impl IssueAction {
    /// The body text to write to a `--body-file`, or `None` when the action
    /// takes no body (close/reopen/edit).
    pub fn body(&self) -> Option<&str> {
        match self {
            Self::Create { body, .. } | Self::Comment { body, .. } => Some(body),
            _ => None,
        }
    }

    /// The gh argument vector. `body_file` is the path passed to `--body-file`
    /// for actions that take a body (see [`Self::body`]); ignored otherwise.
    pub fn build_args(&self, body_file: Option<&str>) -> Vec<String> {
        let s = |x: &str| x.to_string();
        // Append `--body-file <path>` when a body-carrying action has a path.
        let push_body = |args: &mut Vec<String>| {
            if let Some(path) = body_file {
                args.push(s("--body-file"));
                args.push(s(path));
            }
        };
        match self {
            Self::Create { title, .. } => {
                let mut args = vec![s("issue"), s("create"), s("--title"), title.clone()];
                push_body(&mut args);
                args
            }
            Self::Comment { number, .. } => {
                let mut args = vec![s("issue"), s("comment"), number.to_string()];
                push_body(&mut args);
                args
            }
            Self::Close { number } => vec![s("issue"), s("close"), number.to_string()],
            Self::Reopen { number } => vec![s("issue"), s("reopen"), number.to_string()],
            Self::EditLabels {
                number,
                add,
                remove,
            } => edit_args(*number, add, "--add-label", remove, "--remove-label"),
            Self::EditAssignees {
                number,
                add,
                remove,
            } => edit_args(*number, add, "--add-assignee", remove, "--remove-assignee"),
        }
    }

    /// Imperative phrase for a confirm prompt, e.g. "Close issue #5".
    pub fn describe(&self) -> String {
        match self {
            Self::Create { .. } => "Create issue".to_string(),
            Self::Comment { number, .. } => format!("Comment on issue #{number}"),
            Self::Close { number } => format!("Close issue #{number}"),
            Self::Reopen { number } => format!("Reopen issue #{number}"),
            Self::EditLabels { number, .. } => format!("Edit labels on issue #{number}"),
            Self::EditAssignees { number, .. } => format!("Edit assignees on issue #{number}"),
        }
    }
}

/// Build an `issue edit N` command with repeated add/remove flags.
fn edit_args(
    number: u64,
    add: &[String],
    add_flag: &str,
    remove: &[String],
    remove_flag: &str,
) -> Vec<String> {
    let mut args = vec!["issue".to_string(), "edit".to_string(), number.to_string()];
    for a in add {
        args.push(add_flag.to_string());
        args.push(a.clone());
    }
    for r in remove {
        args.push(remove_flag.to_string());
        args.push(r.clone());
    }
    args
}

/// Parse the issue number out of a `gh issue create` URL (`…/issues/<n>`).
pub fn parse_created_issue_number(stdout: &str) -> Option<u64> {
    stdout
        .split("/issues/")
        .nth(1)?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

/// Toast text for a successful action, using gh's output when useful.
pub fn success_message(action: &IssueAction, stdout: &str) -> String {
    match action {
        IssueAction::Create { .. } => match parse_created_issue_number(stdout) {
            Some(n) => format!("Created issue #{n}"),
            None => "Created issue".to_string(),
        },
        IssueAction::Comment { number, .. } => format!("Commented on issue #{number}"),
        IssueAction::Close { number } => format!("Closed issue #{number}"),
        IssueAction::Reopen { number } => format!("Reopened issue #{number}"),
        IssueAction::EditLabels { number, .. } => format!("Updated labels on issue #{number}"),
        IssueAction::EditAssignees { number, .. } => {
            format!("Updated assignees on issue #{number}")
        }
    }
}

// ── async execution ──────────────────────────────────────────────────────

/// Outcome of a completed issue action: the action and gh's result (success
/// text or error message).
pub type IssueActionOutcome = (IssueAction, Result<String, String>);

/// Background runner for one issue action at a time.
#[derive(Default)]
pub struct IssueActionRunner {
    rx: Option<Receiver<IssueActionOutcome>>,
}

impl IssueActionRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Spawn `action` in the background. A body (if any) is written to a temp
    /// file and passed via `--body-file`, then removed. No-op if one is already
    /// in flight.
    pub fn start(&mut self, repo_path: &str, action: IssueAction) {
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
    pub fn poll(&mut self) -> Option<IssueActionOutcome> {
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
                    IssueAction::Close { number: 0 },
                    Err("issue action worker exited".to_string()),
                ))
            }
        }
    }
}

/// Temp path for the `--body-file`. Actions are serialized (one at a time), so
/// a single fixed path is safe.
fn body_file_path() -> PathBuf {
    std::env::temp_dir().join("keifu").join("issue-body.md")
}

fn run_action(repo_path: &str, action: &IssueAction) -> Result<String, String> {
    let result = run_action_inner(repo_path, action);
    if let IssueAction::Create {
        attachment: Some(path),
        ..
    } = action
    {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn run_action_inner(repo_path: &str, action: &IssueAction) -> Result<String, String> {
    let prepared_body = match action {
        IssueAction::Create {
            body,
            attachment: Some(path),
            ..
        } => Some(body_with_attachment(
            body,
            &upload_attachment(repo_path, path)?,
        )),
        _ => None,
    };

    // Write the body to a temp file when the action carries one.
    let body_path = match prepared_body.as_deref().or_else(|| action.body()) {
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

fn upload_attachment(repo_path: &str, path: &std::path::Path) -> Result<String, String> {
    let Some(path) = path.to_str() else {
        return Err("Clipboard image path is not valid UTF-8".to_string());
    };
    let out = gh::run(repo_path, &["image", path], ATTACHMENT_TIMEOUT)?;
    if !out.success {
        let detail = if out.stderr.is_empty() {
            "gh image failed".to_string()
        } else {
            out.stderr
        };
        return Err(format!(
            "Clipboard upload failed; install/configure gh-image \
             (`gh extension install drogers0/gh-image`): {}",
            detail.lines().next().unwrap_or("gh image failed")
        ));
    }
    out.stdout
        .lines()
        .map(str::trim)
        .find(|line| line.contains("github.com/user-attachments/"))
        .map(str::to_string)
        .ok_or_else(|| "gh image returned no GitHub attachment URL".to_string())
}

fn body_with_attachment(body: &str, markdown: &str) -> String {
    if body.trim().is_empty() {
        markdown.to_string()
    } else {
        format!("{}\n\n{}", body.trim_end(), markdown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── command construction ─────────────────────────────────────────

    #[test]
    fn create_args_include_title_and_body_file() {
        let a = IssueAction::Create {
            title: "Crash on open".to_string(),
            body: "steps".to_string(),
            attachment: None,
        };
        assert_eq!(a.body(), Some("steps"));
        assert_eq!(
            a.build_args(Some("/tmp/keifu/issue-body.md")),
            vec![
                "issue",
                "create",
                "--title",
                "Crash on open",
                "--body-file",
                "/tmp/keifu/issue-body.md",
            ]
        );
    }

    #[test]
    fn create_without_body_file_omits_flag() {
        let a = IssueAction::Create {
            title: "t".to_string(),
            body: String::new(),
            attachment: None,
        };
        assert_eq!(a.build_args(None), vec!["issue", "create", "--title", "t"]);
        // Shell-special chars pass through untouched (never a shell).
        let tricky = IssueAction::Create {
            title: "fix: \"q\" & $VAR".to_string(),
            body: String::new(),
            attachment: None,
        };
        assert_eq!(tricky.build_args(None)[3], "fix: \"q\" & $VAR");
    }

    #[test]
    fn comment_args_include_number_and_body_file() {
        let a = IssueAction::Comment {
            number: 42,
            body: "a comment".to_string(),
        };
        assert_eq!(a.body(), Some("a comment"));
        assert_eq!(
            a.build_args(Some("/p")),
            vec!["issue", "comment", "42", "--body-file", "/p"]
        );
    }

    #[test]
    fn close_and_reopen_take_no_body() {
        let close = IssueAction::Close { number: 7 };
        assert_eq!(close.body(), None);
        assert_eq!(close.build_args(Some("/p")), vec!["issue", "close", "7"]);

        let reopen = IssueAction::Reopen { number: 7 };
        assert_eq!(reopen.body(), None);
        assert_eq!(reopen.build_args(None), vec!["issue", "reopen", "7"]);
    }

    #[test]
    fn edit_labels_emits_repeated_add_and_remove_flags() {
        let a = IssueAction::EditLabels {
            number: 3,
            add: vec!["bug".to_string(), "p1".to_string()],
            remove: vec!["wontfix".to_string()],
        };
        assert_eq!(a.body(), None);
        assert_eq!(
            a.build_args(None),
            vec![
                "issue",
                "edit",
                "3",
                "--add-label",
                "bug",
                "--add-label",
                "p1",
                "--remove-label",
                "wontfix",
            ]
        );
    }

    #[test]
    fn edit_labels_add_only_and_remove_only() {
        let add_only = IssueAction::EditLabels {
            number: 1,
            add: vec!["x".to_string()],
            remove: vec![],
        };
        assert_eq!(
            add_only.build_args(None),
            vec!["issue", "edit", "1", "--add-label", "x"]
        );
        let remove_only = IssueAction::EditLabels {
            number: 1,
            add: vec![],
            remove: vec!["y".to_string()],
        };
        assert_eq!(
            remove_only.build_args(None),
            vec!["issue", "edit", "1", "--remove-label", "y"]
        );
        // No-op edit is just the bare command.
        let noop = IssueAction::EditLabels {
            number: 1,
            add: vec![],
            remove: vec![],
        };
        assert_eq!(noop.build_args(None), vec!["issue", "edit", "1"]);
    }

    #[test]
    fn edit_assignees_uses_assignee_flags() {
        let a = IssueAction::EditAssignees {
            number: 9,
            add: vec!["alice".to_string(), "bob".to_string()],
            remove: vec!["carol".to_string()],
        };
        assert_eq!(
            a.build_args(None),
            vec![
                "issue",
                "edit",
                "9",
                "--add-assignee",
                "alice",
                "--add-assignee",
                "bob",
                "--remove-assignee",
                "carol",
            ]
        );
    }

    // ── describe / success message ───────────────────────────────────

    #[test]
    fn describe_gives_imperative_confirm_text() {
        assert_eq!(
            IssueAction::Close { number: 5 }.describe(),
            "Close issue #5"
        );
        assert_eq!(
            IssueAction::Reopen { number: 5 }.describe(),
            "Reopen issue #5"
        );
        assert_eq!(
            IssueAction::Create {
                title: "t".into(),
                body: String::new(),
                attachment: None,
            }
            .describe(),
            "Create issue"
        );
    }

    #[test]
    fn created_issue_number_parsing() {
        assert_eq!(
            parse_created_issue_number("https://github.com/o/r/issues/123\n"),
            Some(123)
        );
        assert_eq!(parse_created_issue_number("no url here"), None);
    }

    #[test]
    fn success_messages_are_specific() {
        assert_eq!(
            success_message(
                &IssueAction::Create {
                    title: "t".into(),
                    body: String::new(),
                    attachment: None,
                },
                "https://github.com/o/r/issues/50\n"
            ),
            "Created issue #50"
        );
        assert_eq!(
            success_message(&IssueAction::Close { number: 8 }, ""),
            "Closed issue #8"
        );
        assert_eq!(
            success_message(&IssueAction::Reopen { number: 8 }, ""),
            "Reopened issue #8"
        );
        assert_eq!(
            success_message(
                &IssueAction::Comment {
                    number: 8,
                    body: "hi".into()
                },
                ""
            ),
            "Commented on issue #8"
        );
        assert_eq!(
            success_message(
                &IssueAction::EditLabels {
                    number: 8,
                    add: vec![],
                    remove: vec![]
                },
                ""
            ),
            "Updated labels on issue #8"
        );
    }

    #[test]
    fn attachment_markdown_is_appended_without_extra_leading_space() {
        let image = "![clipboard-image.png](https://github.com/user-attachments/assets/id)";
        assert_eq!(body_with_attachment("", image), image);
        assert_eq!(
            body_with_attachment("Repro steps\n", image),
            format!("Repro steps\n\n{image}")
        );
    }
}
