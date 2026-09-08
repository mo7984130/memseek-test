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
                    let result = Self::once(ctx, &mut recorder).await;
                    // run 失败已记录,不再进入 validate
                    if let Ok(output) = &result {
                        Self::validate(ctx, output, &mut recorder).await;
                    }
                }
            }
            RunMode::Duration(duration) => {
                let mut remaining = duration;

                while !remaining.is_zero() {
                    let start = Instant::now();

                    let result = Self::once(ctx, &mut recorder).await;

                    let elapsed = start.elapsed();
                    remaining = remaining.saturating_sub(elapsed);

                    // run 失败已记录,不再进入 validate
                    if let Ok(output) = &result {
                        Self::validate(ctx, output, &mut recorder).await;
                    }
                }
            }
        };

        recorder
    }

    /// 执行一次 `run` 并记录耗时/结果,返回本轮产出。
    /// `Err` 已在记录时计为失败,调用方不再进入 validate。
    async fn once(ctx: &S::Ctx, recorder: &mut Recorder) -> Result<S::Output, S::Error> {
        let start = Instant::now();
        let result = S::run(ctx).await;

        recorder.record_duration(start.elapsed());
        recorder.record_result(&result);
        result
    }

    /// 校验 run 的成功产出;`Err` 同样记为一次 validate 失败。
    async fn validate(ctx: &S::Ctx, output: &S::Output, recorder: &mut Recorder) {
        let ret = S::validate(ctx, output).await;
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
