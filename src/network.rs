//! Async network operations: fetch, push with background threading.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;

use crate::config::RefreshConfig;
use crate::git::operations::{fetch_origin, push_to_origin};

/// Manages async fetch/push operations and auto-refresh timers.
pub struct NetworkManager {
    fetch_receiver: Option<Receiver<Result<(), String>>>,
    fetch_silent: bool,
    push_receiver: Option<Receiver<Result<(), String>>>,
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

    pub fn is_busy(&self) -> bool {
        self.is_fetching() || self.is_pushing()
    }

    /// Start a background fetch.
    pub fn start_fetch(&mut self, repo_path: &str, show_message: bool, silent: bool) -> Option<String> {
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let result = fetch_origin(&path).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.fetch_receiver = Some(rx);
        self.fetch_silent = silent;
        if show_message {
            Some("Fetching from origin...".to_string())
        } else {
            None
        }
    }

    /// Start a background push.
    pub fn start_push(&mut self, repo_path: &str) -> String {
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        thread::spawn(move || {
            let result = push_to_origin(&path).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.push_receiver = Some(rx);
        "Pushing to origin...".to_string()
    }

    /// Poll fetch receiver for completion.
    pub fn poll_fetch(&mut self) -> Option<Result<(), String>> {
        let rx = self.fetch_receiver.as_ref()?;
        let result = rx.try_recv().ok()?;
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
        let result = rx.try_recv().ok()?;
        self.push_receiver = None;
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
        if self.is_fetching() {
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
