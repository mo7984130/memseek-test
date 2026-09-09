use std::{
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use tracing::warn;

use crate::{error::ScenarioError, recorder::Recorder, scenario::Scenario};

#[derive(Clone, Copy, Debug)]
pub enum RunMode {
    Times(u64),
    Duration(Duration),
}

/// 并发任务身份,由 Manager 分片时分配,使用者只读。
///
/// `index` 为该任务编号(范围 `0..total`);每个并发任务拥有唯一的
/// `index`,可用它做账号等参数化(`format!("loadtest_{}", index + 1)`)。
/// `round` 为全局运行编号:preset 阶段从共享计数器取号后,
/// 该任务内所有轮次的 `run`/`validate` 均携带同一编号,
/// 可用于全局唯一命名(`format!("data_{}", round)`)或日志定位。
#[derive(Clone, Copy, Debug)]
pub struct TaskIndex {
    /// 当前任务编号,范围 `0..total`
    pub index: usize,
    /// 并发任务总数
    pub total: usize,
    /// 全局运行编号(preset 取号,任务内三阶段共享)
    pub round: usize,
}

impl TaskIndex {
    pub const fn new(index: usize, total: usize, round: usize) -> Self {
        Self {
            index,
            total,
            round,
        }
    }
}

pub struct RunnerConfig {
    pub mode: RunMode,
    /// 任务编号(Manager 分片时分配)
    pub task_index: usize,
    /// 并发任务总数
    pub task_total: usize,
    /// 全局运行编号计数器(Manager 创建,所有并发任务共享,无锁)。
    /// preset 阶段取号,该任务内各轮 run/validate 复用同一编号。
    pub round_counter: std::sync::Arc<AtomicUsize>,
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
        // preset 阶段先取号(所有任务共享受共享计数器),
        // 该任务内各轮 run/validate 复用同一 round。
        let round = self.config.round_counter.fetch_add(1, Ordering::Relaxed);
        let task = TaskIndex::new(self.config.task_index, self.config.task_total, round);
        let mut recorder = Recorder::from_config(&self.config);

        // preset 阶段:每个任务执行一次,产出注入后续每轮 run/validate。
        // 失败时该任务直接中止,记一次失败,不计入耗时分布。
        let preset = match S::preset(ctx, &task).await {
            Ok(preset) => preset,
            Err(err) => {
                recorder.record_result(&Err::<(), S::Error>(err));
                return recorder;
            }
        };

        match self.config.mode {
            RunMode::Times(times) => {
                for _ in 0..times {
                    let result = Self::once(ctx, &task, &preset, &mut recorder).await;
                    // run 失败已记录,不再进入 validate
                    if let Ok(output) = &result {
                        Self::validate(ctx, &task, &preset, output, &mut recorder).await;
                    }
                }
            }
            RunMode::Duration(duration) => {
                let mut remaining = duration;

                while !remaining.is_zero() {
                    let start = Instant::now();

                    let result = Self::once(ctx, &task, &preset, &mut recorder).await;

                    let elapsed = start.elapsed();
                    remaining = remaining.saturating_sub(elapsed);

                    // run 失败已记录,不再进入 validate
                    if let Ok(output) = &result {
                        Self::validate(ctx, &task, &preset, output, &mut recorder).await;
                    }
                }
            }
        };

        recorder
    }

    /// 执行一次 `run` 并记录耗时/结果,返回本轮产出。
    /// `Err` 已在记录时计为失败,调用方不再进入 validate。
    async fn once(
        ctx: &S::Ctx,
        task: &TaskIndex,
        preset: &S::Preset,
        recorder: &mut Recorder,
    ) -> Result<S::Output, S::Error> {
        let start = Instant::now();
        let result = S::run(ctx, task, preset).await;

        recorder.record_duration(start.elapsed());
        recorder.record_result(&result);
        result
    }

    /// 校验 run 的成功产出;`Err` 同样记为一次 validate 失败。
    async fn validate(
        ctx: &S::Ctx,
        task: &TaskIndex,
        preset: &S::Preset,
        output: &S::Output,
        recorder: &mut Recorder,
    ) {
        let ret = S::validate(ctx, task, preset, output).await;
        match ret {
            Ok(validated) => {
                recorder.record_validate(validated);
            }
            Err(err) => {
                warn!("{err:?}");
                recorder.record_validate(false);
            }
        }
    }
}
