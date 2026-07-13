use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use git2::Repository;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

const DEBOUNCE_MS: u64 = 50;
const MAX_BATCH_WAIT_MS: u64 = 500;
const MIN_REFRESH_INTERVAL_MS: u64 = 1000;

pub struct FsWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    repo: Option<Repository>,
    last_event_time: Option<Instant>,
    dirty_since: Option<Instant>,
    last_refresh_time: Option<Instant>,
    dirty: bool,
    disconnected: bool,
    repo_path: PathBuf,
    git_dir: PathBuf,
}

/// A watcher still being constructed on a background thread.
///
/// Registering recursive inotify watches walks every directory in the
/// working tree, which takes hundreds of milliseconds on large repos —
/// far too slow for the pre-first-frame path.
pub struct PendingFsWatcher {
    rx: Receiver<Option<FsWatcher>>,
}

impl PendingFsWatcher {
    /// Returns `Some` once construction has finished (`Some(None)` when
    /// watching is unavailable); `None` while the thread is still working.
    pub fn try_take(&mut self) -> Option<Option<FsWatcher>> {
        match self.rx.try_recv() {
            Ok(watcher) => Some(watcher),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }
}

impl FsWatcher {
    /// Construct a watcher on a background thread; poll the returned
    /// handle with `try_take` to install it once ready.
    pub fn spawn(repo_path: &Path) -> PendingFsWatcher {
        let path = repo_path.to_path_buf();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(FsWatcher::new(&path));
        });
        PendingFsWatcher { rx }
    }

    pub fn new(repo_path: &Path) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(tx, notify::Config::default()).ok()?;
        watcher.watch(repo_path, RecursiveMode::Recursive).ok()?;

        let repo = Repository::open(repo_path).ok();
        let git_dir = repo
            .as_ref()
            .map(|r| r.path().to_path_buf())
            .unwrap_or_else(|| repo_path.join(".git"));

        Some(Self {
            _watcher: watcher,
            rx,
            repo,
            last_event_time: None,
            dirty_since: None,
            last_refresh_time: None,
            dirty: false,
            disconnected: false,
            repo_path: repo_path.to_path_buf(),
            git_dir,
        })
    }

    pub fn poll(&mut self) -> PollResult {
        if self.disconnected {
            return PollResult::Idle;
        }

        loop {
            match self.rx.try_recv() {
                Ok(Ok(event)) => {
                    if self.is_relevant(&event) {
                        self.last_event_time = Some(Instant::now());
                        if self.dirty_since.is_none() {
                            self.dirty_since = Some(Instant::now());
                        }
                        self.dirty = true;
                    }
                }
                Ok(Err(_)) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    return PollResult::Disconnected;
                }
            }
        }

        if self.dirty {
            let debounce_elapsed = self
                .last_event_time
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(DEBOUNCE_MS));
            let max_wait_elapsed = self
                .dirty_since
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(MAX_BATCH_WAIT_MS));

            if debounce_elapsed || max_wait_elapsed {
                if let Some(last) = self.last_refresh_time {
                    if last.elapsed() < Duration::from_millis(MIN_REFRESH_INTERVAL_MS) {
                        return PollResult::Idle;
                    }
                }
                self.dirty = false;
                self.last_event_time = None;
                self.dirty_since = None;
                self.last_refresh_time = Some(Instant::now());
                return PollResult::Refresh;
            }
        }
        PollResult::Idle
    }

    fn is_relevant(&self, event: &Event) -> bool {
        event.paths.iter().any(|path| {
            if let Ok(rel) = path.strip_prefix(&self.git_dir) {
                let s = rel.to_string_lossy();
                s.starts_with("refs")
                    || s == "HEAD"
                    || s == "FETCH_HEAD"
                    || s == "MERGE_HEAD"
                    || s == "REBASE_HEAD"
            } else {
                self.is_tracked_path(path)
            }
        })
    }

    fn is_tracked_path(&self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.repo_path) else {
            return false;
        };
        let Some(repo) = &self.repo else {
            return true;
        };
        !repo.is_path_ignored(rel).unwrap_or(false)
    }
}

pub enum PollResult {
    Idle,
    Refresh,
    Disconnected,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn spawned_watcher_becomes_available() {
        let tempdir = tempfile::tempdir().unwrap();
        git2::Repository::init(tempdir.path()).unwrap();

        let mut pending = FsWatcher::spawn(tempdir.path());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match pending.try_take() {
                Some(watcher) => {
                    assert!(watcher.is_some(), "watcher construction failed");
                    break;
                }
                None if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                None => panic!("watcher construction did not finish in 5s"),
            }
        }
    }
}
