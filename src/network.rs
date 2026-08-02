//! Async network operations: fetch, pull, push with background threading.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::RefreshConfig;
use crate::git::operations::{
    fetch_all_controlled, fetch_remote_controlled, pull_controlled, push_current_controlled,
    push_delete_controlled, push_head_to_remote_controlled, push_set_upstream_controlled,
    NetworkCommandControl, OpOutcome, PullMode,
};
use crate::git::Credentials;

/// Maximum time a network operation may go without observable transport
/// progress before cancellation is requested automatically.
pub const NETWORK_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);

/// Whether this platform exposes the process-group signaling needed to cancel
/// Git without force-killing only its direct child. Non-Unix builds retain
/// transport low-speed bounds but do not advertise or accept cancellation.
pub const NETWORK_CANCELLATION_SUPPORTED: bool = cfg!(unix);

/// The user-facing kind of the one network operation keifu permits at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkOperation {
    Fetch,
    Pull,
    Push,
}

/// Why cancellation was requested for an active operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationReason {
    User,
    InactivityTimeout,
}

pub(crate) type PullIntegrationAcknowledgement = Sender<Result<(), CancellationReason>>;

/// Lifecycle phase exposed to the status bar while an operation is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPhase {
    Running,
    /// Pull transport finished; local index/worktree integration is now
    /// non-cancellable and repository-mutating actions must be gated.
    Integrating,
    Cancelling(CancellationReason),
}

/// Snapshot consumed by the UI; it is derived from the worker lifecycle rather
/// than maintained as separate presentation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkStatus {
    pub operation: NetworkOperation,
    pub phase: NetworkPhase,
}

/// Monotonic transport counters. Any advancing field refreshes the inactivity
/// deadline; repeated snapshots do not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetworkProgress {
    pub bytes: u64,
    pub objects: u64,
    pub refs: u64,
}

/// Terminal failure delivered by a network worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkFailure {
    Failed(String),
    Cancelled(CancellationReason),
}

/// Cloneable cooperative-cancellation signal shared with a Git worker.
#[derive(Debug, Clone, Default)]
pub(crate) struct CancellationToken(Arc<AtomicU8>);

impl CancellationToken {
    pub(crate) fn request(&self, reason: CancellationReason) -> bool {
        if !NETWORK_CANCELLATION_SUPPORTED {
            return false;
        }
        let encoded = match reason {
            CancellationReason::User => 1,
            CancellationReason::InactivityTimeout => 2,
        };
        self.0
            .compare_exchange(0, encoded, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn reason(&self) -> Option<CancellationReason> {
        match self.0.load(Ordering::Acquire) {
            1 => Some(CancellationReason::User),
            2 => Some(CancellationReason::InactivityTimeout),
            _ => None,
        }
    }
}

#[cfg(all(test, not(unix)))]
mod non_unix_cancellation_tests {
    use super::*;

    #[test]
    fn cancellation_is_rejected_without_safe_process_group_signaling() {
        assert!(!NETWORK_CANCELLATION_SUPPORTED);
        let started = Instant::now();
        let mut network = NetworkManager::active_for_test(NetworkOperation::Fetch, started);

        assert!(!network.cancel_active(CancellationReason::User));
        assert!(!network.check_inactivity_at(started + NETWORK_INACTIVITY_TIMEOUT));
        assert_eq!(
            network.status(),
            Some(NetworkStatus {
                operation: NetworkOperation::Fetch,
                phase: NetworkPhase::Running,
            })
        );
    }
}

impl std::fmt::Display for NetworkFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(message) => f.write_str(message),
            Self::Cancelled(CancellationReason::User) => f.write_str("cancelled"),
            Self::Cancelled(CancellationReason::InactivityTimeout) => {
                f.write_str("timed out after 60 seconds without progress")
            }
        }
    }
}

/// What a background push should do.
#[derive(Debug, Clone)]
pub enum PushSpec {
    /// Push the current branch to its configured upstream (`git push`).
    Current,
    /// Publish `branch` to `remote`, setting upstream (`git push -u`).
    Publish { remote: String, branch: String },
    /// Push HEAD to an explicit `remote` without changing upstream tracking
    /// (`git push <remote> HEAD`) — chosen when the picked remote isn't the
    /// configured upstream.
    ToRemote { remote: String },
    /// Delete `branch` on `remote` (`git push <remote> --delete <branch>`).
    /// Routed through the push pipeline so it shares the auth-retry + busy-guard
    /// machinery; the UI removes the branch optimistically before dispatch.
    Delete { remote: String, branch: String },
}

/// Manages async fetch/pull/push operations and auto-refresh timers.
pub struct NetworkManager {
    fetch_receiver: Option<Receiver<Result<(), NetworkFailure>>>,
    fetch_silent: bool,
    push_receiver: Option<Receiver<Result<(), NetworkFailure>>>,
    pull_receiver: Option<Receiver<Result<OpOutcome, NetworkFailure>>>,
    active: Option<ActiveOperation>,
    progress_receiver: Option<Receiver<NetworkProgress>>,
    integration_receiver: Option<Receiver<PullIntegrationAcknowledgement>>,
    last_refresh_time: Instant,
    last_fetch_time: Instant,
}

struct ActiveOperation {
    operation: NetworkOperation,
    phase: NetworkPhase,
    last_progress: NetworkProgress,
    last_progress_at: Instant,
    cancellation: CancellationToken,
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
            active: None,
            progress_receiver: None,
            integration_receiver: None,
            last_refresh_time: now,
            last_fetch_time: now,
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.operation == NetworkOperation::Fetch)
    }

    pub fn is_pushing(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.operation == NetworkOperation::Push)
    }

    pub fn is_pulling(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.operation == NetworkOperation::Pull)
    }

    pub fn is_busy(&self) -> bool {
        self.is_fetching() || self.is_pushing() || self.is_pulling()
    }

    /// Whether pull has crossed into its non-cancellable local mutation phase.
    pub fn is_integrating(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.phase == NetworkPhase::Integrating)
    }

    pub fn can_cancel(&self) -> bool {
        NETWORK_CANCELLATION_SUPPORTED
            && self
                .active
                .as_ref()
                .is_some_and(|active| active.phase == NetworkPhase::Running)
    }

    /// Current operation lifecycle snapshot, when network work is active.
    pub fn status(&self) -> Option<NetworkStatus> {
        self.active.as_ref().map(|active| NetworkStatus {
            operation: active.operation,
            phase: active.phase,
        })
    }

    /// Record a monotonic worker-progress snapshot at `observed_at`.
    pub fn record_progress_at(&mut self, progress: NetworkProgress, observed_at: Instant) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.phase != NetworkPhase::Running {
            return;
        }
        let advanced = progress.bytes > active.last_progress.bytes
            || progress.objects > active.last_progress.objects
            || progress.refs > active.last_progress.refs;
        if advanced {
            active.last_progress.bytes = active.last_progress.bytes.max(progress.bytes);
            active.last_progress.objects = active.last_progress.objects.max(progress.objects);
            active.last_progress.refs = active.last_progress.refs.max(progress.refs);
            active.last_progress_at = observed_at;
        }
    }

    /// Request timeout cancellation when the current inactivity window has
    /// elapsed. Returns true only for the transition into cancellation.
    pub fn check_inactivity_at(&mut self, now: Instant) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if active.phase != NetworkPhase::Running
            || now.saturating_duration_since(active.last_progress_at) < NETWORK_INACTIVITY_TIMEOUT
        {
            return false;
        }
        let reason = CancellationReason::InactivityTimeout;
        if !active.cancellation.request(reason) {
            return false;
        }
        active.phase = NetworkPhase::Cancelling(reason);
        true
    }

    /// Request cancellation of the active operation. Returns true only for the
    /// first request accepted by a running operation.
    pub fn cancel_active(&mut self, reason: CancellationReason) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if active.phase != NetworkPhase::Running || !active.cancellation.request(reason) {
            return false;
        }
        active.phase = NetworkPhase::Cancelling(reason);
        true
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn active_for_test(operation: NetworkOperation, started_at: Instant) -> Self {
        let mut manager = Self::new();
        manager.activate_for_test(operation, started_at);
        manager
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn activate_for_test(&mut self, operation: NetworkOperation, started_at: Instant) {
        let cancellation = CancellationToken::default();
        self.active = Some(ActiveOperation {
            operation,
            phase: NetworkPhase::Running,
            last_progress: NetworkProgress::default(),
            last_progress_at: started_at,
            cancellation,
        });
        let (_tx, rx) = mpsc::channel();
        match operation {
            NetworkOperation::Fetch => self.fetch_receiver = Some(rx),
            NetworkOperation::Push => self.push_receiver = Some(rx),
            NetworkOperation::Pull => {
                let (_tx, rx) = mpsc::channel();
                self.pull_receiver = Some(rx);
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn activate_integrating_pull_for_test(&mut self) {
        self.activate_for_test(NetworkOperation::Pull, Instant::now());
        self.active.as_mut().expect("activated pull").phase = NetworkPhase::Integrating;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn finish_cancelled_for_test(&mut self, reason: CancellationReason) {
        let Some(operation) = self.active.as_ref().map(|active| active.operation) else {
            return;
        };
        match operation {
            NetworkOperation::Fetch => {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(Err(NetworkFailure::Cancelled(reason)));
                self.fetch_receiver = Some(rx);
            }
            NetworkOperation::Push => {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(Err(NetworkFailure::Cancelled(reason)));
                self.push_receiver = Some(rx);
            }
            NetworkOperation::Pull => {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(Err(NetworkFailure::Cancelled(reason)));
                self.pull_receiver = Some(rx);
            }
        }
    }

    fn begin_operation(
        &mut self,
        operation: NetworkOperation,
    ) -> (
        CancellationToken,
        Sender<NetworkProgress>,
        Sender<PullIntegrationAcknowledgement>,
    ) {
        let cancellation = CancellationToken::default();
        let (progress_tx, progress_rx) = mpsc::channel();
        let (integration_tx, integration_rx) = mpsc::channel();
        self.active = Some(ActiveOperation {
            operation,
            phase: NetworkPhase::Running,
            last_progress: NetworkProgress::default(),
            last_progress_at: Instant::now(),
            cancellation: cancellation.clone(),
        });
        self.progress_receiver = Some(progress_rx);
        self.integration_receiver = Some(integration_rx);
        (cancellation, progress_tx, integration_tx)
    }

    /// Drain worker progress and transition a stalled operation into automatic
    /// cancellation. Returns true when the user-visible phase changed.
    pub fn tick(&mut self) -> bool {
        self.tick_at(Instant::now())
    }

    fn tick_at(&mut self, now: Instant) -> bool {
        let progress: Vec<_> = self
            .progress_receiver
            .as_ref()
            .map(|receiver| receiver.try_iter().collect())
            .unwrap_or_default();
        for snapshot in progress {
            self.record_progress_at(snapshot, now);
        }
        let integration_barriers: Vec<_> = self
            .integration_receiver
            .as_ref()
            .map(|receiver| receiver.try_iter().collect())
            .unwrap_or_default();
        let mut integration_started = false;
        for acknowledge in integration_barriers {
            if let Some(active) = self.active.as_mut() {
                if active.operation == NetworkOperation::Pull {
                    match active.phase {
                        NetworkPhase::Running => {
                            active.phase = NetworkPhase::Integrating;
                            integration_started = true;
                            let _ = acknowledge.send(Ok(()));
                        }
                        NetworkPhase::Cancelling(reason) => {
                            let _ = acknowledge.send(Err(reason));
                        }
                        NetworkPhase::Integrating => {}
                    }
                }
            }
        }
        integration_started || self.check_inactivity_at(now)
    }

    fn finish_operation(&mut self, operation: NetworkOperation) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.operation == operation)
        {
            self.active = None;
            self.progress_receiver = None;
            self.integration_receiver = None;
        }
    }

    /// Start a background fetch from `remote`.
    pub fn start_fetch(
        &mut self,
        repo_path: &str,
        remote: &str,
        show_message: bool,
        silent: bool,
        creds: Option<Credentials>,
    ) -> Option<String> {
        let (tx, rx) = mpsc::channel();
        let (cancellation, progress_tx, integration_tx) =
            self.begin_operation(NetworkOperation::Fetch);
        let control =
            NetworkCommandControl::with_integration(cancellation, progress_tx, integration_tx);
        let path = repo_path.to_string();
        let remote_owned = remote.to_string();
        thread::spawn(move || {
            let result = fetch_remote_controlled(&path, &remote_owned, creds.as_ref(), &control);
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
    pub fn start_fetch_all(&mut self, repo_path: &str, creds: Option<Credentials>) -> String {
        let (tx, rx) = mpsc::channel();
        let (cancellation, progress_tx, integration_tx) =
            self.begin_operation(NetworkOperation::Fetch);
        let control =
            NetworkCommandControl::with_integration(cancellation, progress_tx, integration_tx);
        let path = repo_path.to_string();
        thread::spawn(move || {
            let result = fetch_all_controlled(&path, creds.as_ref(), &control);
            let _ = tx.send(result);
        });
        self.fetch_receiver = Some(rx);
        self.fetch_silent = false;
        "Fetching all remotes...".to_string()
    }

    /// Start a background push per `spec`.
    pub fn start_push(
        &mut self,
        repo_path: &str,
        spec: PushSpec,
        creds: Option<Credentials>,
    ) -> String {
        let (tx, rx) = mpsc::channel();
        let (cancellation, progress_tx, integration_tx) =
            self.begin_operation(NetworkOperation::Push);
        let control =
            NetworkCommandControl::with_integration(cancellation, progress_tx, integration_tx);
        let path = repo_path.to_string();
        let message = match &spec {
            PushSpec::Current => "Pushing...".to_string(),
            PushSpec::Publish { remote, branch } => {
                format!("Publishing {branch} to {remote}...")
            }
            PushSpec::ToRemote { remote } => format!("Pushing to {remote}..."),
            PushSpec::Delete { remote, branch } => format!("Deleting {remote}/{branch}..."),
        };
        thread::spawn(move || {
            let result = match spec {
                PushSpec::Current => push_current_controlled(&path, creds.as_ref(), &control),
                PushSpec::Publish { remote, branch } => {
                    push_set_upstream_controlled(&path, &remote, &branch, creds.as_ref(), &control)
                }
                PushSpec::ToRemote { remote } => {
                    push_head_to_remote_controlled(&path, &remote, creds.as_ref(), &control)
                }
                PushSpec::Delete { remote, branch } => {
                    push_delete_controlled(&path, &remote, &branch, creds.as_ref(), &control)
                }
            };
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
        creds: Option<Credentials>,
    ) -> String {
        let (tx, rx) = mpsc::channel();
        let (cancellation, progress_tx, integration_tx) =
            self.begin_operation(NetworkOperation::Pull);
        let control =
            NetworkCommandControl::with_integration(cancellation, progress_tx, integration_tx);
        let path = repo_path.to_string();
        let message = match &remote {
            Some(r) => format!("Pulling from {r}..."),
            None => "Pulling...".to_string(),
        };
        thread::spawn(move || {
            let result = pull_controlled(
                &path,
                remote.as_deref(),
                branch.as_deref(),
                mode,
                creds.as_ref(),
                &control,
            );
            let _ = tx.send(result);
        });
        self.pull_receiver = Some(rx);
        message
    }

    /// Poll fetch receiver for completion. Returns `(result, silent)` where
    /// `silent` marks a background auto-fetch (vs a user-initiated one), so the
    /// caller can decide whether to surface success. Silent *errors* are no
    /// longer suppressed here — the caller shows them as a toast rather than the
    /// full error dialog.
    pub fn poll_fetch(&mut self) -> Option<(Result<(), NetworkFailure>, bool)> {
        let rx = self.fetch_receiver.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            // Worker died without reporting; clear state so fetching
            // doesn't stay stuck "in progress" forever.
            Err(TryRecvError::Disconnected) => Err(NetworkFailure::Failed(
                "fetch worker exited unexpectedly".to_string(),
            )),
        };
        let silent = self.fetch_silent;
        self.fetch_receiver = None;
        self.fetch_silent = false;
        self.finish_operation(NetworkOperation::Fetch);
        Some((result, silent))
    }

    /// Poll push receiver for completion.
    pub fn poll_push(&mut self) -> Option<Result<(), NetworkFailure>> {
        let rx = self.push_receiver.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(NetworkFailure::Failed(
                "push worker exited unexpectedly".to_string(),
            )),
        };
        self.push_receiver = None;
        self.finish_operation(NetworkOperation::Push);
        Some(result)
    }

    /// Poll pull receiver for completion.
    pub fn poll_pull(&mut self) -> Option<Result<OpOutcome, NetworkFailure>> {
        let rx = self.pull_receiver.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(NetworkFailure::Failed(
                "pull worker exited unexpectedly".to_string(),
            )),
        };
        self.pull_receiver = None;
        self.finish_operation(NetworkOperation::Pull);
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

    /// Test-only: complete a fetch synchronously with `result`/`silent`,
    /// without spawning a background thread, so `poll_fetch` immediately
    /// yields it. Lets latch/timer behavior in `update_fetch_status` be
    /// exercised deterministically.
    #[cfg(test)]
    pub(crate) fn complete_fetch_for_test(&mut self, result: Result<(), String>, silent: bool) {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(result.map_err(NetworkFailure::Failed));
        if self.active.is_none() {
            self.activate_for_test(NetworkOperation::Fetch, Instant::now());
        }
        self.fetch_receiver = Some(rx);
        self.fetch_silent = silent;
    }

    /// Test-only: complete a push (or push-based delete) synchronously with
    /// `result`, without spawning a thread, so `poll_push` immediately yields it.
    /// Lets `update_push_status` completion handling (incl. optimistic-delete
    /// restore) be exercised deterministically.
    #[cfg(test)]
    pub(crate) fn complete_push_for_test(&mut self, result: Result<(), String>) {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(result.map_err(NetworkFailure::Failed));
        if self.active.is_none() {
            self.activate_for_test(NetworkOperation::Push, Instant::now());
        }
        self.push_receiver = Some(rx);
    }
}

#[cfg(all(test, unix))]
mod pull_integration_race_tests {
    use super::*;

    #[test]
    fn cancellation_before_barrier_acknowledgement_keeps_its_reason() {
        let mut manager = NetworkManager::new();
        let (cancellation, progress_tx, integration_tx) =
            manager.begin_operation(NetworkOperation::Pull);
        let control =
            NetworkCommandControl::with_integration(cancellation, progress_tx, integration_tx);
        assert!(manager.cancel_active(CancellationReason::User));

        let worker = thread::spawn(move || control.begin_pull_integration());
        let deadline = Instant::now() + Duration::from_secs(1);
        while !worker.is_finished() {
            manager.tick();
            assert!(
                Instant::now() < deadline,
                "cancelled integration barrier was never acknowledged"
            );
            thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(
            worker.join().unwrap(),
            Err(NetworkFailure::Cancelled(CancellationReason::User))
        );
    }
}
