use std::{
    marker::PhantomData,
    time::{Duration, Instant},
};

use tracing::warn;

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
                    Self::validate(ctx, &mut recorder).await;
                }
            }
            RunMode::Duration(duration) => {
                let mut remaining = duration;

                while !remaining.is_zero() {
                    let start = Instant::now();

                    Self::once(ctx, &mut recorder).await;

                    let elapsed = start.elapsed();
                    remaining = remaining.saturating_sub(elapsed);

                    Self::validate(ctx, &mut recorder).await;
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

    async fn validate(ctx: &S::Ctx, recorder: &mut Recorder) {
        let ret = S::validate(ctx).await;
        match ret {
            Ok(validated) => {
                recorder.record_validate(validated);
            }
            Err(err) => {
                warn!("{:#?}", err);
                recorder.record_validate(false);
            }
        }
    }
}
