use std::time::Duration;

use crate::runner::RunnerConfig;

pub trait ScenarioRecorder {
    fn from_config(config: &RunnerConfig) -> Self;

    fn record(&mut self, duration: Duration);

    fn times(&self) -> u64;
    fn total(&self) -> Duration;
    fn avg(&self) -> Duration;
    fn min(&self) -> Duration;
    fn max(&self) -> Duration;

    fn p50(&mut self) -> Duration;
    fn p95(&mut self) -> Duration;
    fn p99(&mut self) -> Duration;

    fn merge(&mut self, other: Self);
}
