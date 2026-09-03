use std::{
    marker::PhantomData,
    time::{Duration, Instant},
};

use crate::{error::ScenarioError, recorder::Recorder, scenario::Scenario};

#[derive(Clone, Copy, Debug)]
pub enum RunMode {
    Times(u64),
    Duration(Duration),
}

pub struct RunnerConfig {
    pub mode: RunMode,
}

pub struct ScenarioRunner<S> {
    config: RunnerConfig,
    _marker: PhantomData<S>,
}

impl<S> ScenarioRunner<S>
where
    S: Scenario,
    S::Error: ScenarioError,
{
    pub fn new(config: RunnerConfig) -> Self {
        Self {
            config,
            _marker: PhantomData,
        }
    }

    pub async fn run(&self, ctx: &S::Ctx) -> Recorder {
        let mut recorder = Recorder::from_config(&self.config);
        match self.config.mode {
            RunMode::Times(times) => {
                for _ in 0..times {
                    Self::once(ctx, &mut recorder).await;
                }
            }
            RunMode::Duration(deadline) => {
                let dl = Instant::now() + deadline;
                loop {
                    Self::once(ctx, &mut recorder).await;
                    if Instant::now() >= dl {
                        break;
                    }
                }
            }
        };

        recorder
    }

    async fn once(ctx: &S::Ctx, recorder: &mut Recorder) {
        let start = Instant::now();
        let result = S::run(ctx).await;

        recorder.record_duration(start.elapsed());
        recorder.record_result(&result);
    }
}
