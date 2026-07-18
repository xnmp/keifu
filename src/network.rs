//! Async network operations: fetch, pull, push with background threading.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use crate::config::RefreshConfig;
use crate::git::operations::{
    fetch_all, fetch_remote, pull, push_current, push_set_upstream, OpOutcome, PullMode,
};

/// What a background push should do.
#[derive(Debug, Clone)]
pub enum PushSpec {
    /// Push the current branch to its configured upstream (`git push`).
    Current,
    /// Publish `branch` to `remote`, setting upstream (`git push -u`).
    Publish { remote: String, branch: String },
}

/// Manages async fetch/pull/push operations and auto-refresh timers.
pub struct NetworkManager {
    fetch_receiver: Option<Receiver<Result<(), String>>>,
    fetch_silent: bool,
    push_receiver: Option<Receiver<Result<(), String>>>,
    pull_receiver: Option<Receiver<Result<OpOutcome, String>>>,
    last_refresh_time: Instant,
    last_fetch_time: Instant,
}

/// Result of polling network operations.
#[derive(Debug, Default)]
pub struct NetworkEvents {
    /// Fetch completed successfully — App should refresh.
    pub fetch_completed: bool,
    /// Push completed successfully — App should refresh.
    pub push_completed: bool,
    /// Should trigger auto-fetch.
    pub should_auto_fetch: bool,
    /// Should trigger auto-refresh (local only).
    pub should_auto_refresh: bool,
    /// Error to show to user.
    pub error: Option<String>,
    /// Status message.
    pub message: Option<String>,
}

impl Default for NetworkManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkManager {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            fetch_receiver: None,
            fetch_silent: false,
            push_receiver: None,
            pull_receiver: None,
            last_refresh_time: now,
            last_fetch_time: now,
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.fetch_receiver.is_some()
    }

    pub fn is_pushing(&self) -> bool {
        self.push_receiver.is_some()
    }

    pub fn is_pulling(&self) -> bool {
        self.pull_receiver.is_some()
    }

    pub fn is_busy(&self) -> bool {
        self.is_fetching() || self.is_pushing() || self.is_pulling()
    }

    /// Start a background fetch from `remote`.
    pub fn start_fetch(
        &mut self,
        repo_path: &str,
        remote: &str,
        show_message: bool,
        silent: bool,
    ) -> Option<String> {
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        let remote_owned = remote.to_string();
        thread::spawn(move || {
            let result = fetch_remote(&path, &remote_owned).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.fetch_receiver = Some(rx);
        self.fetch_silent = silent;
        if show_message {
            Some(format!("Fetching from {remote}..."))
        } else {
            None
        }
    }

    /// Start a background fetch from every configured remote (`git fetch
    /// --all`). Shares the fetch receiver, so completion flows through the same
    /// `poll_fetch` / refresh path as a single-remote fetch.
    pub fn start_fetch_all(&mut self, repo_path: &str) -> String {
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let result = fetch_all(&path).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.fetch_receiver = Some(rx);
        self.fetch_silent = false;
        "Fetching all remotes...".to_string()
    }

    /// Start a background push per `spec`.
    pub fn start_push(&mut self, repo_path: &str, spec: PushSpec) -> String {
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        let message = match &spec {
            PushSpec::Current => "Pushing...".to_string(),
            PushSpec::Publish { remote, branch } => {
                format!("Publishing {branch} to {remote}...")
            }
        };
        thread::spawn(move || {
            let result = match spec {
                PushSpec::Current => push_current(&path),
                PushSpec::Publish { remote, branch } => {
                    push_set_upstream(&path, &remote, &branch)
                }
            }
            .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.push_receiver = Some(rx);
        message
    }

    /// Start a background pull. `remote`/`branch` = `None` runs a bare
    /// `git pull` (using the configured upstream); an explicit remote runs
    /// `git pull <remote> <branch>`.
    pub fn start_pull(
        &mut self,
        repo_path: &str,
        remote: Option<String>,
        branch: Option<String>,
        mode: PullMode,
    ) -> String {
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        let message = match &remote {
            Some(r) => format!("Pulling from {r}..."),
            None => "Pulling...".to_string(),
        };
        thread::spawn(move || {
            let result = pull(&path, remote.as_deref(), branch.as_deref(), mode)
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.pull_receiver = Some(rx);
        message
    }

    /// Poll fetch receiver for completion.
    pub fn poll_fetch(&mut self) -> Option<Result<(), String>> {
        let rx = self.fetch_receiver.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            // Worker died without reporting; clear state so fetching
            // doesn't stay stuck "in progress" forever.
            Err(TryRecvError::Disconnected) => Err("fetch worker exited unexpectedly".to_string()),
        };
        let silent = self.fetch_silent;
        self.fetch_receiver = None;
        self.fetch_silent = false;
        match result {
            Ok(()) => Some(Ok(())),
            Err(e) if !silent => Some(Err(e)),
            Err(_) => None, // Silent mode: suppress
        }
    }

    /// Poll push receiver for completion.
    pub fn poll_push(&mut self) -> Option<Result<(), String>> {
        let rx = self.push_receiver.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("push worker exited unexpectedly".to_string()),
        };
        self.push_receiver = None;
        Some(result)
    }

    /// Poll pull receiver for completion.
    pub fn poll_pull(&mut self) -> Option<Result<OpOutcome, String>> {
        let rx = self.pull_receiver.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("pull worker exited unexpectedly".to_string()),
        };
        self.pull_receiver = None;
        Some(result)
    }

    /// Reset both timers (call after manual refresh/fetch).
    pub fn reset_timers(&mut self) {
        let now = Instant::now();
        self.last_refresh_time = now;
        self.last_fetch_time = now;
    }

    /// Check if auto-refresh or auto-fetch should trigger.
    pub fn check_auto_timers(&self, config: &RefreshConfig) -> NetworkEvents {
        let mut events = NetworkEvents::default();
        if self.is_busy() {
            return events;
        }

        let now = Instant::now();

        if config.auto_fetch
            && now.duration_since(self.last_fetch_time).as_secs() >= config.fetch_interval
        {
            events.should_auto_fetch = true;
            return events;
        }

        if config.auto_refresh
            && now.duration_since(self.last_refresh_time).as_secs() >= config.refresh_interval
        {
            events.should_auto_refresh = true;
        }

        events
    }

    /// Mark that a local refresh just happened.
    pub fn mark_refreshed(&mut self) {
        self.last_refresh_time = Instant::now();
    }
}
