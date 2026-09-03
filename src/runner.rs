use std::marker::PhantomData;
use std::time::Instant;

use crate::recorders::error_recorder::ErrorRecorder;
use crate::recorders::recorder::ScenarioRecorder;
use crate::scenario::Scenario;

pub struct RunnerConfig {
    pub times: u64,
}

pub struct RunnerResult<R: ScenarioRecorder> {
    pub recorder: R,
    pub error_recorder: ErrorRecorder,
}
impl<R: ScenarioRecorder> RunnerResult<R> {
    pub fn merge(&mut self, other: Self) {
        self.recorder.merge(other.recorder);
        self.error_recorder.merge(other.error_recorder);
    }
}

pub struct ScenarioRunner<R: ScenarioRecorder> {
    config: RunnerConfig,
    _marker: PhantomData<R>,
}
impl<R: ScenarioRecorder> ScenarioRunner<R> {
    pub fn new(config: RunnerConfig) -> Self {
        Self {
            config,
            _marker: PhantomData,
        }
    }

    pub async fn run<S>(&mut self, ctx: &S::Ctx) -> RunnerResult<R>
    where
        S: Scenario,
    {
        let mut recorder = R::from_config(&self.config);
        let mut error_recorder = ErrorRecorder::new();

        for _ in 0..self.config.times {
            let start = Instant::now();

            let result = S::run(ctx).await;

            recorder.record(start.elapsed());
            error_recorder.record(&result);
        }

        RunnerResult {
            recorder,
            error_recorder,
        }
    }
}
