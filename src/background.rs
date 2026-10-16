//! A bounded, latest-request worker. One running job, one replacement slot,
//! one result slot; invalidating a result never spawns overlapping workers.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone)]
pub struct Cancellation {
    generation: u64,
    current: Arc<AtomicU64>,
}

impl Cancellation {
    pub fn is_cancelled(&self) -> bool {
        self.current.load(Ordering::Relaxed) != self.generation
    }
}

type Job<T> = Box<dyn FnOnce() -> Result<T, String> + Send>;
struct State<T> {
    job: Option<(u64, Job<T>)>,
    result: Option<(u64, Result<T, String>)>,
    stopped: bool,
}

pub struct LatestTask<T> {
    shared: Arc<(Mutex<State<T>>, Condvar)>,
    generation: u64,
    pending: bool,
    started: bool,
    current: Arc<AtomicU64>,
}

impl<T> Default for LatestTask<T> {
    fn default() -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(State {
                    job: None,
                    result: None,
                    stopped: false,
                }),
                Condvar::new(),
            )),
            generation: 0,
            pending: false,
            started: false,
            current: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<T: Send + 'static> LatestTask<T> {
    /// Embedded callers without an event loop can use the identical job and
    /// delivery contract synchronously. The interactive app always uses a worker.
    pub fn submit(
        &mut self,
        threaded: bool,
        job: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) {
        self.submit_cancellable(threaded, move |_| job());
    }

    pub fn submit_cancellable(
        &mut self,
        threaded: bool,
        job: impl FnOnce(Cancellation) -> Result<T, String> + Send + 'static,
    ) {
        self.generation = self.generation.wrapping_add(1);
        self.current.store(self.generation, Ordering::Relaxed);
        let cancel = Cancellation {
            generation: self.generation,
            current: Arc::clone(&self.current),
        };
        let job = move || job(cancel);
        self.pending = true;
        let (lock, wake) = &*self.shared;
        if !threaded {
            lock.lock().unwrap().result = Some((self.generation, job()));
            return;
        }
        {
            let mut state = lock.lock().unwrap();
            state.result = None;
            state.job = Some((self.generation, Box::new(job)));
        }
        if !self.started {
            self.started = true;
            let shared = Arc::clone(&self.shared);
            let current = Arc::clone(&self.current);
            std::thread::spawn(move || {
                let (lock, wake) = &*shared;
                loop {
                    let (generation, job) = {
                        let mut state = lock.lock().unwrap();
                        while state.job.is_none() && !state.stopped {
                            state = wake.wait(state).unwrap();
                        }
                        if state.stopped {
                            return;
                        }
                        state.job.take().unwrap()
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                        .unwrap_or_else(|_| Err("Background computation failed".into()));
                    let mut state = lock.lock().unwrap();
                    if current.load(Ordering::Relaxed) == generation {
                        state.result = Some((generation, result));
                    }
                }
            });
        }
        wake.notify_one();
    }

    pub fn take(&mut self) -> Option<Result<T, String>> {
        let result = self.shared.0.lock().unwrap().result.take()?;
        if result.0 != self.generation {
            return None;
        }
        self.pending = false;
        Some(result.1)
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.current.store(self.generation, Ordering::Relaxed);
        self.pending = false;
        let mut state = self.shared.0.lock().unwrap();
        state.job = None;
        state.result = None;
    }
}

impl<T> Drop for LatestTask<T> {
    fn drop(&mut self) {
        self.current.fetch_add(1, Ordering::Relaxed);
        let mut state = self.shared.0.lock().unwrap();
        state.stopped = true;
        state.job = None;
        self.shared.1.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn result<T: Send + 'static>(task: &mut LatestTask<T>) -> Result<T, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = task.take() {
                return result;
            }
            assert!(Instant::now() < deadline, "worker did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn replacements_skip_queued_work_and_never_deliver_a_stale_result() {
        let mut task = LatestTask::default();
        let (started, running) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        task.submit(true, move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
            Ok(1)
        });
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        let (executed, unexpected) = mpsc::channel();
        task.submit(true, move || {
            executed.send(()).unwrap();
            Ok(2)
        });
        task.submit(true, || Ok(3));
        assert!(task.take().is_none());
        release.send(()).unwrap();
        assert_eq!(result(&mut task), Ok(3));
        assert!(
            unexpected.try_recv().is_err(),
            "superseded queued job executed"
        );
        assert!(!task.is_pending());
    }

    #[test]
    fn cancellation_discards_results_and_worker_recovers_after_a_panic() {
        let mut task = LatestTask::<usize>::default();
        task.submit(false, || Ok(1));
        task.cancel();
        assert!(task.take().is_none());
        task.submit(true, || panic!("test worker failure"));
        assert!(result(&mut task).is_err());
        task.submit(true, || Ok(42));
        assert_eq!(result(&mut task), Ok(42));
    }
}
