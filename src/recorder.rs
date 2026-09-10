use std::{borrow::Cow, collections::HashMap, time::Duration};

use hdrhistogram::Histogram;
use tracing::warn;

use crate::{error::ScenarioError, runner::RunnerConfig};

pub struct Recorder {
    histogram: Histogram<u64>,
    total: Duration,

    pub success: u64,
    pub failures: u64,
    pub error_map: HashMap<Cow<'static, str>, u64>,

    pub validate_success: u64,
    pub validate_failures: u64,
    /// 该任务是否因优雅关闭(停止信号)提前结束
    pub interrupted: bool,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            histogram: Histogram::new(3).unwrap(),
            total: Duration::ZERO,
            success: 0,
            failures: 0,
            error_map: HashMap::new(),
            validate_success: 0,
            validate_failures: 0,
            interrupted: false,
        }
    }

    pub fn from_config(_config: &RunnerConfig) -> Self {
        Self::new()
    }

    pub fn total(&self) -> Duration {
        self.total
    }

    pub fn times(&self) -> u64 {
        self.histogram.len()
    }

    pub fn avg(&self) -> Duration {
        if self.histogram.is_empty() {
            return Duration::ZERO;
        }

        Duration::from_micros(self.histogram.mean() as u64)
    }

    pub fn min(&self) -> Duration {
        if self.histogram.is_empty() {
            Duration::ZERO
        } else {
            Duration::from_micros(self.histogram.min())
        }
    }

    pub fn max(&self) -> Duration {
        if self.histogram.is_empty() {
            Duration::ZERO
        } else {
            Duration::from_micros(self.histogram.max())
        }
    }

    pub fn p50(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.50))
    }

    pub fn p95(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.95))
    }

    pub fn p99(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.99))
    }

    pub fn merge(&mut self, other: Self) {
        self.histogram
            .add(other.histogram)
            .expect("merge Recorder failed");
        self.total += other.total;
        self.success += other.success;
        self.failures += other.failures;
        for (kind, count) in other.error_map {
            *self.error_map.entry(kind).or_insert(0) += count;
        }

        self.validate_success += other.validate_success;
        self.validate_failures += other.validate_failures;
        self.interrupted |= other.interrupted;
    }

    pub fn record_duration(&mut self, duration: Duration) {
        self.histogram
            .record(duration.as_micros() as u64)
            .expect("duration is out of histogram range");
        self.total += duration;
    }

    pub fn record_result<T, E>(&mut self, result: &std::result::Result<T, E>)
    where
        E: ScenarioError,
    {
        match result {
            Ok(_) => self.success += 1,
            Err(err) => {
                warn!("{err:?}");
                self.failures += 1;
                self.error_map
                    .entry(err.kind())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }
        }
    }

    pub fn record_validate(&mut self, validate_result: bool) {
        match validate_result {
            true => {
                self.validate_success += 1;
            }
            false => {
                self.validate_failures += 1;
            }
        }
    }

    /// 标记该任务因优雅关闭提前结束(计入报告)。
    pub fn record_interrupted(&mut self) {
        self.interrupted = true;
    }
}
