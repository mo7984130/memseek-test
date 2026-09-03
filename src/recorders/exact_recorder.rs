use std::time::Duration;

use crate::recorders::recorder::ScenarioRecorder;

pub struct ExactRecorder {
    durations: Vec<Duration>,
    is_sorted: bool,
}

impl ExactRecorder {
    fn new(capacity: u64) -> Self {
        Self {
            durations: Vec::with_capacity(capacity as usize),
            is_sorted: true,
        }
    }

    fn percentile(&mut self, percentile: f64) -> Duration {
        if self.durations.is_empty() {
            return Duration::ZERO;
        }

        if !self.is_sorted {
            self.durations.sort_unstable();
        }

        let index = ((self.durations.len() - 1) as f64 * percentile).round() as usize;

        self.durations[index]
    }
}

impl ScenarioRecorder for ExactRecorder {
    fn from_config(config: &crate::runner::RunnerConfig) -> Self {
        Self::new(config.times)
    }

    fn record(&mut self, duration: Duration) {
        self.durations.push(duration);
        self.is_sorted = false;
    }

    fn total(&self) -> Duration {
        self.durations.iter().sum()
    }

    fn times(&self) -> u64 {
        self.durations.len() as u64
    }

    fn min(&self) -> Duration {
        self.durations
            .iter()
            .min()
            .copied()
            .unwrap_or(Duration::ZERO)
    }

    fn max(&self) -> Duration {
        self.durations
            .iter()
            .max()
            .copied()
            .unwrap_or(Duration::ZERO)
    }

    fn avg(&self) -> Duration {
        if self.durations.is_empty() {
            return Duration::ZERO;
        }

        let total: Duration = self.durations.iter().sum();

        total / self.durations.len() as u32
    }

    fn p50(&mut self) -> Duration {
        self.percentile(0.50)
    }

    fn p95(&mut self) -> Duration {
        self.percentile(0.95)
    }

    fn p99(&mut self) -> Duration {
        self.percentile(0.99)
    }

    fn merge(&mut self, other: Self) {
        self.durations.extend(other.durations);
        self.is_sorted = false;
    }
}
