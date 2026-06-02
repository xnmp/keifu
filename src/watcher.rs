use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use git2::Repository;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

const DEBOUNCE_MS: u64 = 50;
const MIN_REFRESH_INTERVAL_MS: u64 = 1000;

pub struct FsWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    last_event_time: Option<Instant>,
    last_refresh_time: Option<Instant>,
    dirty: bool,
    repo_path: PathBuf,
    git_dir: PathBuf,
}

impl FsWatcher {
    pub fn new(repo_path: &Path) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(tx, notify::Config::default()).ok()?;
        watcher.watch(repo_path, RecursiveMode::Recursive).ok()?;

        Some(Self {
            _watcher: watcher,
            rx,
            last_event_time: None,
            last_refresh_time: None,
            dirty: false,
            repo_path: repo_path.to_path_buf(),
            git_dir: repo_path.join(".git"),
        })
    }

    pub fn poll(&mut self) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(Ok(event)) => {
                    if self.is_relevant(&event) {
                        self.last_event_time = Some(Instant::now());
                        self.dirty = true;
                    }
                }
                Ok(Err(_)) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        if self.dirty {
            if let Some(t) = self.last_event_time {
                if t.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                    if let Some(last) = self.last_refresh_time {
                        if last.elapsed() < Duration::from_millis(MIN_REFRESH_INTERVAL_MS) {
                            return false;
                        }
                    }
                    self.dirty = false;
                    self.last_event_time = None;
                    self.last_refresh_time = Some(Instant::now());
                    return true;
                }
            }
        }
        false
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
        let Ok(repo) = Repository::open(&self.repo_path) else {
            return true;
        };
        !repo.is_path_ignored(rel).unwrap_or(false)
    }
}
