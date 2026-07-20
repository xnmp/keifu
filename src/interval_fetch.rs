//! `IntervalFetch<T>`: a generic interval-polled background fetcher.
//!
//! This is the reusable form of the byte-identical spawn+deadline poll loop that
//! `pr::PrFetch` and `merged_branch_fetch::MergedBranchFetch` each hand-rolled
//! (issue #65). Both fetched a value from GitHub on a coarse interval, ran it on
//! a background thread, and polled a channel — differing only in the produced
//! type and the exact `gh` invocation.
//!
//! `IntervalFetch<T>` factors that out: it is parameterized by a poll interval
//! and a *producer* closure `Fn(&str) -> Result<T, String>` that (in production)
//! routes through [`crate::gh::run`]. The producer is injectable, so the fetcher
//! is unit-testable without spawning a real `gh`.
//!
//! **Error convention (issue #65):** unlike the old fetchers — which mapped every
//! failure to an empty value and silently rendered empty sets — `poll` surfaces a
//! `Result`. A missing `gh` binary (or any producer error) is therefore
//! observable, so the caller can latch/toast it once per episode instead of
//! wiping the last-good data on a transient failure.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// A background producer: given the repo path, produce a value or an error
/// string. Routed through `gh::run` in production; injected in tests. `Send +
/// Sync` so it can be shared across the spawn boundary and re-run each interval.
pub type Producer<T> = Arc<dyn Fn(&str) -> Result<T, String> + Send + Sync>;

/// Background fetcher that runs `producer` on a worker thread at most once per
/// `interval`, never blocking the UI thread. Fetches immediately on the first
/// `maybe_start`, then waits a full interval between fetches (the interval also
/// serves as the back-off after a failure — no retry storm).
pub struct IntervalFetch<T> {
    interval: Duration,
    producer: Producer<T>,
    receiver: Option<Receiver<Result<T, String>>>,
    last_fetch: Option<Instant>,
}

impl<T: Send + 'static> IntervalFetch<T> {
    /// Build a fetcher that runs `producer` every `interval`.
    pub fn new(
        interval: Duration,
        producer: impl Fn(&str) -> Result<T, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            interval,
            producer: Arc::new(producer),
            receiver: None,
            last_fetch: None,
        }
    }

    /// Make the next `maybe_start` fetch immediately, ignoring the interval. A
    /// fetch already in flight is untouched (no duplicate spawn).
    pub fn force(&mut self) {
        self.last_fetch = None;
    }

    /// Spawn a fetch when none is in flight and one is due (immediately on the
    /// first call, then on the interval).
    pub fn maybe_start(&mut self, repo_path: &str) {
        if self.receiver.is_some() {
            return;
        }
        let due = self.last_fetch.is_none_or(|t| t.elapsed() >= self.interval);
        if !due {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = repo_path.to_string();
        let producer = Arc::clone(&self.producer);
        thread::spawn(move || {
            let _ = tx.send(producer(&path));
        });
        self.receiver = Some(rx);
    }

    /// Poll for a completed fetch. Returns the producer's `Result` once on
    /// completion (`Ok(value)` or `Err(message)`), else `None`. Records the
    /// completion time so the next fetch waits a full interval — even after a
    /// failure, so a persistently-failing producer can't spin.
    pub fn poll(&mut self) -> Option<Result<T, String>> {
        let rx = self.receiver.as_ref()?;
        match rx.try_recv() {
            Ok(r) => {
                self.receiver = None;
                self.last_fetch = Some(Instant::now());
                Some(r)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // Worker died without sending; back off until the next interval
                // and surface it as an error so it can be latched like any other.
                self.receiver = None;
                self.last_fetch = Some(Instant::now());
                Some(Err("fetch worker exited".to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Block-poll until the in-flight fetch completes, so the delivery assertions
    /// don't race the worker thread. Bounded so a bug can't hang the suite.
    fn drain<T: Send + 'static>(f: &mut IntervalFetch<T>) -> Result<T, String> {
        for _ in 0..10_000 {
            if let Some(r) = f.poll() {
                return r;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("fetch never completed");
    }

    #[test]
    fn delivers_producer_value() {
        let mut f = IntervalFetch::new(Duration::from_secs(300), |path: &str| {
            Ok(format!("fetched:{path}"))
        });
        f.maybe_start("repo");
        assert_eq!(drain(&mut f).unwrap(), "fetched:repo");
    }

    #[test]
    fn surfaces_producer_error() {
        // The gh-missing case: the producer returns Err, and `poll` surfaces it
        // instead of substituting an empty value.
        let mut f: IntervalFetch<String> =
            IntervalFetch::new(Duration::from_secs(300), |_| Err("gh not available".to_string()));
        f.maybe_start("repo");
        assert_eq!(drain(&mut f).unwrap_err(), "gh not available");
    }

    #[test]
    fn interval_gates_a_second_fetch() {
        // A long interval: after one completed fetch, an immediate `maybe_start`
        // must not spawn a second run (the producer runs exactly once).
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let mut f = IntervalFetch::new(Duration::from_secs(300), move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        f.maybe_start("repo");
        drain(&mut f).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Not due yet (300s interval) → no second spawn, nothing to poll.
        f.maybe_start("repo");
        assert!(f.poll().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "interval must gate the refetch");
    }

    #[test]
    fn force_ignores_the_interval() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let mut f = IntervalFetch::new(Duration::from_secs(300), move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        f.maybe_start("repo");
        drain(&mut f).unwrap();
        // Force overrides the not-yet-due interval, so the next start refetches.
        f.force();
        f.maybe_start("repo");
        drain(&mut f).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn only_one_fetch_in_flight_at_a_time() {
        // A second `maybe_start` while one is in flight must not spawn a duplicate.
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let mut f = IntervalFetch::new(Duration::from_secs(300), move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            Ok(())
        });
        f.maybe_start("repo");
        f.maybe_start("repo"); // in flight → ignored
        drain(&mut f).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
