use std::time::Duration;

use hdrhistogram::Histogram;

use crate::recorders::recorder::ScenarioRecorder;

pub struct HdrRecorder {
    histogram: Histogram<u64>,
    total: Duration,
}

impl HdrRecorder {
    pub fn new() -> Self {
        Self {
            histogram: Histogram::new(3).unwrap(),
            total: Duration::ZERO,
        }
    }
}

impl Default for HdrRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl ScenarioRecorder for HdrRecorder {
    fn from_config(_config: &crate::runner::RunnerConfig) -> Self {
        Self::new()
    }

    fn record(&mut self, duration: Duration) {
        self.histogram
            .record(duration.as_micros() as u64)
            .expect("duration is out of histogram range");
        self.total += duration;
    }

    fn total(&self) -> Duration {
        self.total
    }

    fn times(&self) -> u64 {
        self.histogram.len()
    }

    fn avg(&self) -> Duration {
        if self.histogram.is_empty() {
            return Duration::ZERO;
        }

        Duration::from_micros(self.histogram.mean() as u64)
    }

    fn min(&self) -> Duration {
        if self.histogram.is_empty() {
            Duration::ZERO
        } else {
            Duration::from_micros(self.histogram.min())
        }
    }

    fn max(&self) -> Duration {
        if self.histogram.is_empty() {
            Duration::ZERO
        } else {
            Duration::from_micros(self.histogram.max())
        }
    }

    fn p50(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.50))
    }

    fn p95(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.95))
    }

    fn p99(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.99))
    }

    fn merge(&mut self, other: Self) {
        self.histogram
            .add(other.histogram)
            .expect("merge HdrRecorder failed")
    }
}
