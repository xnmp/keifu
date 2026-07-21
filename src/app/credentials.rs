//! HTTPS credential prompt + retry orchestration (issue #33).
//!
//! When a background network op fails with an HTTPS auth failure, we prompt the
//! user in-TUI for a username then a (masked) password/token, cache them for the
//! session keyed by host, and retry the exact same op with the credentials
//! supplied via `GIT_ASKPASS`. Cached credentials are attached transparently to
//! later ops on the same host, so the user is asked at most once per host.

use super::*;

/// Cap on credential prompts per auth episode, so a persistently-wrong token
/// can't loop the prompt forever — after this many tries the raw error shows.
const MAX_AUTH_PROMPTS: u32 = 3;

impl App {
    // ── Dispatch with credentials attached ───────────────────────────────

    /// Start a network op, attaching any cached credentials for its target
    /// host, and record it as in-flight so a later auth failure can retry it.
    /// `attempts` carries the running prompt count across retries (0 for a
    /// fresh, user-initiated op).
    pub(crate) fn dispatch_net_op(&mut self, op: RetryableOp, attempts: u32) {
        let host = self.op_host(&op);
        let creds = host
            .as_ref()
            .and_then(|h| self.credentials.get(h).cloned());
        let had_creds = creds.is_some();
        let silent = matches!(&op, RetryableOp::Fetch { silent: true, .. });

        let message = match op.clone() {
            RetryableOp::Fetch { remote, show_message, silent } => {
                self.network
                    .start_fetch(&self.repo_path, &remote, show_message, silent, creds)
            }
            RetryableOp::FetchAll => {
                Some(self.network.start_fetch_all(&self.repo_path, creds))
            }
            RetryableOp::Push(spec) => {
                Some(self.network.start_push(&self.repo_path, spec, creds))
            }
            RetryableOp::Pull { remote, branch, mode } => Some(
                self.network
                    .start_pull(&self.repo_path, remote, branch, mode, creds),
            ),
        };

        // Fetch progress is a one-shot event notification → toast (a silent
        // auto-fetch yields no message, so it stays quiet). Push/pull progress
        // is a sticky message that persists for the whole in-flight op.
        let is_fetch = matches!(&op, RetryableOp::Fetch { .. } | RetryableOp::FetchAll);
        self.in_flight_op = Some(InFlightOp { op, host, had_creds, silent, attempts });
        if let Some(msg) = message {
            if is_fetch {
                self.toast(crate::toast::ToastKind::Info, msg);
            } else {
                self.set_progress_message(msg);
            }
        }
    }

    /// Resolve the host an op authenticates against, for credential-cache
    /// lookup. `None` when the remote or its URL can't be resolved (e.g. an SSH
    /// remote, which this flow doesn't handle).
    fn op_host(&self, op: &RetryableOp) -> Option<String> {
        let remote = match op {
            RetryableOp::Fetch { remote, .. } => Some(remote.clone()),
            RetryableOp::FetchAll => self
                .repo
                .head_upstream_remote()
                .or_else(|| self.repo.remotes().into_iter().next()),
            RetryableOp::Push(PushSpec::Current) => self.repo.head_upstream_remote(),
            RetryableOp::Push(PushSpec::Publish { remote, .. }) => Some(remote.clone()),
            RetryableOp::Push(PushSpec::ToRemote { remote }) => Some(remote.clone()),
            RetryableOp::Push(PushSpec::Delete { remote, .. }) => Some(remote.clone()),
            RetryableOp::Pull { remote: Some(r), .. } => Some(r.clone()),
            RetryableOp::Pull { remote: None, .. } => self.repo.head_upstream_remote(),
        };
        remote
            .and_then(|r| self.repo.remote_url(&r))
            .and_then(|url| url_host(&url))
    }

    // ── Prompt on auth failure ───────────────────────────────────────────

    /// On an HTTPS auth failure, open the credential prompt and return `true`
    /// (the caller must NOT also show the error). Returns `false` for any other
    /// error, a silent background op, or once the prompt cap is reached — the
    /// caller then surfaces the error normally.
    pub(crate) fn try_prompt_credentials(
        &mut self,
        err: &str,
        flight: Option<InFlightOp>,
    ) -> bool {
        if !is_https_auth_failure(err) {
            return false;
        }
        let Some(flight) = flight else {
            return false;
        };
        // Never interrupt a silent background auto-fetch with a modal prompt.
        if flight.silent {
            return false;
        }
        if flight.attempts >= MAX_AUTH_PROMPTS {
            return false;
        }

        let parsed = extract_auth_url(err);
        let host = parsed
            .as_ref()
            .map(|u| u.host.clone())
            .or_else(|| flight.host.clone());
        let Some(host) = host else {
            return false;
        };

        // A failure carrying credentials means the cached ones are stale.
        let prev_user = self.credentials.get(&host).map(|c| c.username.clone());
        if flight.had_creds {
            self.credentials.remove(&host);
            self.toast(
                crate::toast::ToastKind::Error,
                "Authentication failed — re-enter credentials",
            );
        }

        let prefill = parsed
            .and_then(|u| u.user)
            .or(prev_user)
            .unwrap_or_default();

        self.pending_auth = Some(PendingAuth {
            op: flight.op,
            host,
            username: None,
            attempts: flight.attempts + 1,
        });
        self.open_auth_username_prompt(prefill);
        true
    }

    fn open_auth_username_prompt(&mut self, prefill: String) {
        let host = self
            .pending_auth
            .as_ref()
            .map(|p| p.host.clone())
            .unwrap_or_default();
        self.mode = AppMode::Input {
            title: format!("Username for {host}"),
            input: prefill,
            action: InputAction::AuthUsername,
        };
    }

    /// Advance from the username step to the (masked) password step.
    pub(crate) fn auth_advance_to_password(&mut self, username: String) {
        let host = match self.pending_auth.as_mut() {
            Some(pa) => {
                pa.username = Some(username);
                pa.host.clone()
            }
            // No pending auth (shouldn't happen) — bail back to Normal.
            None => {
                self.mode = AppMode::Normal;
                return;
            }
        };
        self.mode = AppMode::Input {
            title: format!("Password / token for {host}"),
            input: String::new(),
            action: InputAction::AuthPassword,
        };
    }

    /// Finish the password step: cache the credentials and retry the op.
    pub(crate) fn auth_submit_password(&mut self, password: String) {
        let Some(pa) = self.pending_auth.take() else {
            self.mode = AppMode::Normal;
            return;
        };
        let username = pa.username.unwrap_or_default();
        self.credentials
            .insert(pa.host.clone(), Credentials { username, password });
        self.mode = AppMode::Normal;
        self.dispatch_net_op(pa.op, pa.attempts);
    }

    /// Cancel an in-progress credential prompt (Esc at either step).
    pub(crate) fn auth_cancel(&mut self) {
        // Cancelling abandons the pending op for good. If that op was an
        // optimistic remote-branch deletion, the branch was hidden on
        // dispatch and the completion handler that would have unhidden it
        // will never run — drop the pending-hide and refresh so the branch
        // reappears instead of staying wrongly deleted-looking until restart.
        if let Some(pending) = self.pending_auth.take() {
            if let RetryableOp::Push(crate::network::PushSpec::Delete { remote, branch }) =
                &pending.op
            {
                self.pending_remote_deletions
                    .remove(&format!("{remote}/{branch}"));
                self.toast(
                    crate::toast::ToastKind::Info,
                    format!("Remote deletion of {remote}/{branch} cancelled"),
                );
                let _ = self.refresh(false);
            }
        }
        self.mode = AppMode::Normal;
    }

    // ── Paste handling ───────────────────────────────────────────────────

    /// Route a bracketed-paste chunk to the right sink for the current mode:
    /// single-line inputs (credential prompt, create-branch, search, …) get a
    /// sanitized one-line chunk; multi-line compose editors keep newlines.
    pub fn handle_paste(&mut self, text: String) -> Result<()> {
        match &self.mode {
            AppMode::Input { .. } | AppMode::CommandPalette { .. } => {
                let cleaned = sanitize_paste_single_line(&text);
                if !cleaned.is_empty() {
                    self.handle_action(Action::InputPaste(cleaned))?;
                }
            }
            AppMode::PrCompose { .. } => {
                let cleaned = sanitize_paste_multiline(&text);
                self.pr_editor.insert_str(&cleaned);
            }
            AppMode::IssueCompose { .. } => {
                let cleaned = sanitize_paste_multiline(&text);
                self.issue_editor.insert_str(&cleaned);
            }
            AppMode::Normal if self.editing_commit_message => {
                let cleaned = sanitize_paste_multiline(&text);
                self.commit_editor.insert_str(&cleaned);
            }
            _ => {}
        }
        Ok(())
    }
}

/// Strip a pasted chunk to a single line: remove every control character,
/// including newlines and tabs. Terminals may deliver a trailing newline with a
/// paste; this drops it so a token doesn't submit or wrap.
pub fn sanitize_paste_single_line(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// Sanitize a pasted chunk for a multi-line editor: keep newlines, drop `\r`
/// and every other control character.
pub fn sanitize_paste_multiline(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{sanitize_paste_multiline, sanitize_paste_single_line};

    #[test]
    fn single_line_strips_newlines_tabs_and_controls() {
        assert_eq!(
            sanitize_paste_single_line("ghp_abc\n123\t\r"),
            "ghp_abc123"
        );
        // Embedded bell / null are removed; ordinary text survives.
        assert_eq!(sanitize_paste_single_line("a\u{7}b\u{0}c"), "abc");
        assert_eq!(sanitize_paste_single_line("plain-token"), "plain-token");
    }

    #[test]
    fn multiline_keeps_newlines_but_drops_other_controls() {
        assert_eq!(
            sanitize_paste_multiline("line1\r\nline2\tend\u{7}"),
            "line1\nline2end"
        );
        assert_eq!(sanitize_paste_multiline("a\nb\nc"), "a\nb\nc");
    }
}
