//! Lightweight performance counters
//!
//! Operations are recorded by the main loop and app operations. Slow ones
//! are emitted as tracing events as they happen, and an aggregate summary
//! is logged on exit (both require --log-file).

use std::collections::HashMap;
use std::time::Duration;

/// Operations at or above this duration are logged as they happen
const SLOW_THRESHOLD: Duration = Duration::from_millis(10);

#[derive(Debug, Default, Clone, Copy)]
pub struct Aggregate {
    pub count: u64,
    pub total: Duration,
    pub max: Duration,
}

impl Aggregate {
    fn record(&mut self, duration: Duration) {
        self.count += 1;
        self.total += duration;
        if duration > self.max {
            self.max = duration;
        }
    }

    pub fn avg(&self) -> Duration {
        if self.count == 0 {
            Duration::ZERO
        } else {
            self.total / self.count as u32
        }
    }
}

#[derive(Debug, Default)]
pub struct PerfStats {
    ops: HashMap<&'static str, Aggregate>,
}

impl PerfStats {
    pub fn record(&mut self, name: &'static str, duration: Duration) {
        self.ops.entry(name).or_default().record(duration);
        if duration >= SLOW_THRESHOLD {
            tracing::debug!(
                op = name,
                ms = duration.as_millis() as u64,
                "slow operation"
            );
        }
    }

    pub fn ops(&self) -> impl Iterator<Item = (&'static str, &Aggregate)> {
        self.ops.iter().map(|(name, agg)| (*name, agg))
    }

    /// Emit an aggregate summary to the log (called on exit)
    pub fn log_summary(&self) {
        let ms = |d: Duration| (d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0;
        let mut ops: Vec<_> = self.ops().collect();
        ops.sort_by_key(|(name, _)| *name);
        for (name, agg) in ops {
            tracing::info!(
                op = name,
                count = agg.count,
                avg_ms = ms(agg.avg()),
                max_ms = ms(agg.max),
                "perf summary"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_aggregates() {
        let mut perf = PerfStats::default();
        perf.record("op", Duration::from_millis(20));
        perf.record("op", Duration::from_millis(40));

        let agg = perf.ops().find(|(n, _)| *n == "op").unwrap().1;
        assert_eq!(agg.count, 2);
        assert_eq!(agg.max, Duration::from_millis(40));
        assert_eq!(agg.avg(), Duration::from_millis(30));
    }
}
