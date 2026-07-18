//! Background author-avatar downloader.
//!
//! A single persistent worker thread pulls author emails off a queue, resolves
//! each to a GitHub/Gravatar URL, downloads it via `curl` into the on-disk
//! cache, and reports the outcome. The UI thread never blocks: it enqueues
//! emails and drains results each frame (mirroring the other background ops).
//!
//! Results are two-state: `Ready` (a PNG sits in the cache, decoded lazily at
//! render time) or `Missing` (404 / error / no `curl` → a deterministic
//! fallback disc). Negative results are cached on disk as empty `.missing`
//! marker files with a 7-day retry window.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::SystemTime;

use crate::avatar::{
    cache_dir, cache_missing_path, cache_png_path, missing_is_expired, resolve_avatar_url,
    MISSING_TTL,
};

/// Resolved state of an author's avatar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarState {
    /// A decoded-able image is in the disk cache.
    Ready,
    /// No avatar (404/error/no curl) — render a fallback disc.
    Missing,
}

/// Owns the avatar worker thread and the resolved-state cache.
pub struct AvatarFetch {
    /// Send author emails to the worker (None once the worker is gone).
    job_tx: Option<Sender<String>>,
    /// Receive `(email, state)` results from the worker.
    result_rx: Receiver<(String, AvatarState)>,
    /// Emails already queued (dedup — never enqueue twice).
    requested: HashSet<String>,
    /// Resolved state per email.
    resolved: HashMap<String, AvatarState>,
    /// Keep the worker handle alive for the process lifetime.
    _worker: thread::JoinHandle<()>,
}

impl AvatarFetch {
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<String>();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || worker_loop(job_rx, result_tx));
        Self {
            job_tx: Some(job_tx),
            result_rx,
            requested: HashSet::new(),
            resolved: HashMap::new(),
            _worker: worker,
        }
    }

    /// Queue `email` for download if it hasn't been seen. Empty emails and
    /// duplicates are ignored.
    pub fn request(&mut self, email: &str) {
        if email.trim().is_empty() || self.requested.contains(email) {
            return;
        }
        self.requested.insert(email.to_string());
        if let Some(tx) = &self.job_tx {
            // A dead worker (send error) just means avatars stay pending; the
            // fallback path still renders discs elsewhere.
            let _ = tx.send(email.to_string());
        }
    }

    /// Drain completed downloads into the resolved map. Returns whether any new
    /// result arrived (so the caller can trigger a redraw).
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((email, state)) = self.result_rx.try_recv() {
            self.resolved.insert(email, state);
            changed = true;
        }
        changed
    }

    /// The resolved state for `email`, or `None` while still pending.
    pub fn state_of(&self, email: &str) -> Option<AvatarState> {
        self.resolved.get(email).copied()
    }
}

impl Default for AvatarFetch {
    fn default() -> Self {
        Self::new()
    }
}

/// Worker thread: process each queued email in turn until the sender is dropped.
fn worker_loop(job_rx: Receiver<String>, result_tx: Sender<(String, AvatarState)>) {
    let dir = cache_dir();
    if let Some(d) = &dir {
        let _ = std::fs::create_dir_all(d);
    }
    for email in job_rx {
        let state = process_email(dir.as_deref(), &email);
        if result_tx.send((email, state)).is_err() {
            break; // UI gone
        }
    }
}

/// Resolve one email: consult the disk cache, else download. Pure I/O, off the
/// UI thread.
fn process_email(dir: Option<&Path>, email: &str) -> AvatarState {
    let Some(dir) = dir else {
        return AvatarState::Missing; // no cache dir → fallback forever
    };
    let png = cache_png_path(dir, email);
    if png.exists() {
        return AvatarState::Ready;
    }
    let missing = cache_missing_path(dir, email);
    if let Ok(mtime) = std::fs::metadata(&missing).and_then(|m| m.modified()) {
        if !missing_is_expired(mtime, SystemTime::now(), MISSING_TTL) {
            return AvatarState::Missing; // still within the retry window
        }
    }

    let Some(url) = resolve_avatar_url(email) else {
        touch_missing(&missing);
        return AvatarState::Missing;
    };

    // Download to a temp path, then atomically move into place only if it looks
    // like a real image (curl -f already rejects 404s, but a magic-byte sniff
    // guards against a 200 error page).
    let tmp = png.with_extension("tmp");
    if download(&url, &tmp) && is_image_file(&tmp) && std::fs::rename(&tmp, &png).is_ok() {
        return AvatarState::Ready;
    }
    let _ = std::fs::remove_file(&tmp);
    touch_missing(&missing);
    AvatarState::Missing
}

/// Fetch `url` into `out` with curl. `-f` fails on HTTP errors (so 404s don't
/// write a body); curl's absence returns false (→ fallback). Never blocks the
/// UI — this runs on the worker thread.
fn download(url: &str, out: &Path) -> bool {
    Command::new("curl")
        .args(["-fsL", "--max-time", "10", "-o"])
        .arg(out)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Cheap PNG/JPEG magic-byte check, so a non-image response isn't cached as one.
fn is_image_file(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let png = bytes.starts_with(&[0x89, b'P', b'N', b'G']);
    let jpeg = bytes.starts_with(&[0xFF, 0xD8, 0xFF]);
    png || jpeg
}

/// Create/refresh an empty `.missing` marker so the negative result is cached
/// with a fresh mtime (the retry window keys off it).
fn touch_missing(path: &Path) {
    let _ = std::fs::write(path, []);
}
