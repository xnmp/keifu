use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

const DEBOUNCE_MS: u64 = 50;

pub struct FsWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    last_event_time: Option<Instant>,
    dirty: bool,
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
            dirty: false,
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
                    self.dirty = false;
                    self.last_event_time = None;
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
                true
            }
        })
    }
}
