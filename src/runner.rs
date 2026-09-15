use std::{
    borrow::Cow,
    marker::PhantomData,
    time::{Duration, Instant},
};

#[cfg(feature = "tui")]
use crate::progress::Progress;
use crate::{
    error::ScenarioError,
    recorder::Recorder,
    scenario::{Scenario, SetupMode},
    shutdown::Shutdown,
};

#[derive(Clone, Copy, Debug)]
pub enum RunMode {
    Times(u64),
    Duration(Duration),
}

/// 并发任务身份,由 Manager 分片时分配,使用者只读。
///
/// `index` 为该任务编号(范围 `0..total`);每个并发任务拥有唯一的
/// `index`,可用它做账号等参数化(`format!("loadtest_{}", index + 1)`)。
/// `round` 为全局运行编号:每次执行(含 `setup`)从共享计数器取号,
/// 同一轮内的 `setup`/`run`/`validate` 携带同一编号,
/// 可用于全局唯一命名(`format!("data_{}", round)`)或日志定位。
#[derive(Clone, Copy, Debug)]
pub struct TaskIndex {
    /// 当前任务编号,范围 `0..total`
    pub index: usize,
    /// 并发任务总数
    pub total: usize,
    /// 全局运行编号(本轮编号,每轮递增)
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
    /// 优雅关闭信号;触发后当前轮次完成即停止(不再启动新轮次)
    pub shutdown: Option<Shutdown>,
    /// 实时进度上报句柄(feature `tui`,由 Manager 注入);`None` 表示不上报
    #[cfg(feature = "tui")]
    pub progress: Option<Progress>,
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

    /// 执行并记录一轮 `run`,成功时进入 `validate`。
    async fn run_and_validate(
        ctx: &S::Ctx,
        task: &TaskIndex,
        setup: &S::Setup,
        recorder: &mut Recorder,
    ) {
        let result = Self::once(ctx, task, setup, recorder).await;
        // run 失败已记录,不再进入 validate
        if let Ok(output) = &result {
            Self::validate(ctx, task, setup, output, recorder).await;
        }
    }

    pub async fn run(&self, ctx: &S::Ctx) -> Recorder {
        let mut recorder = Recorder::from_config(&self.config);

        // 优雅关闭:信号到达时已完成当前轮次,不再启动新轮次。
        // 各分支在 setup 后/每轮开始前检查。
        match S::SETUP_MODE {
            SetupMode::Task => {
                if self.is_cancelled() {
                    recorder.record_interrupted();
                    return recorder;
                }
                // setup 任务级一次(round 无轮次含义,固定 0),产出供各轮复用
                let setup_task = TaskIndex::new(self.config.task_index, self.config.task_total, 0);
                #[cfg(feature = "tui")]
                recorder.begin_setup();
                let setup = S::setup(ctx, &setup_task).await;
                #[cfg(feature = "tui")]
                recorder.end_setup();
                let setup = match setup {
                    Ok(setup) => setup,
                    Err(err) => {
                        recorder.record_result(&Err::<(), S::Error>(err));
                        return recorder;
                    }
                };
                // setup 期间收到信号:等 setup 完成后直接退出,不进入 run 循环
                if self.is_cancelled() {
                    recorder.record_interrupted();
                    return recorder;
                }
                match self.config.mode {
                    RunMode::Times(times) => {
                        for round in 0..times {
                            if self.is_cancelled() {
                                recorder.record_interrupted();
                                break;
                            }
                            let task = TaskIndex::new(
                                self.config.task_index,
                                self.config.task_total,
                                round as usize,
                            );
                            Self::run_and_validate(ctx, &task, &setup, &mut recorder).await;
                        }
                    }
                    RunMode::Duration(duration) => {
                        let mut remaining = duration;
                        let mut round = 0usize;

                        while !remaining.is_zero() {
                            if self.is_cancelled() {
                                recorder.record_interrupted();
                                break;
                            }
                            let start = Instant::now();
                            let task = TaskIndex::new(
                                self.config.task_index,
                                self.config.task_total,
                                round,
                            );
                            round += 1;

                            Self::run_and_validate(ctx, &task, &setup, &mut recorder).await;

                            remaining = remaining.saturating_sub(start.elapsed());
                        }
                    }
                }
            }
            SetupMode::Round => {
                // setup 每轮一次:失败则记一次失败并中止任务
                match self.config.mode {
                    RunMode::Times(times) => {
                        for round in 0..times {
                            if self.is_cancelled() {
                                recorder.record_interrupted();
                                break;
                            }
                            let task = TaskIndex::new(
                                self.config.task_index,
                                self.config.task_total,
                                round as usize,
                            );
                            let setup = match S::setup(ctx, &task).await {
                                Ok(setup) => setup,
                                Err(err) => {
                                    recorder.record_result(&Err::<(), S::Error>(err));
                                    return recorder;
                                }
                            };
                            Self::run_and_validate(ctx, &task, &setup, &mut recorder).await;
                        }
                    }
                    RunMode::Duration(duration) => {
                        let mut remaining = duration;
                        let mut round = 0usize;

                        while !remaining.is_zero() {
                            if self.is_cancelled() {
                                recorder.record_interrupted();
                                break;
                            }
                            let start = Instant::now();
                            let task = TaskIndex::new(
                                self.config.task_index,
                                self.config.task_total,
                                round,
                            );
                            round += 1;
                            let setup = match S::setup(ctx, &task).await {
                                Ok(setup) => setup,
                                Err(err) => {
                                    recorder.record_result(&Err::<(), S::Error>(err));
                                    return recorder;
                                }
                            };

                            Self::run_and_validate(ctx, &task, &setup, &mut recorder).await;

                            remaining = remaining.saturating_sub(start.elapsed());
                        }
                    }
                }
            }
        }

        recorder
    }

    /// 是否已收到优雅关闭信号。
    fn is_cancelled(&self) -> bool {
        self.config
            .shutdown
            .as_ref()
            .is_some_and(|s| s.is_cancelled())
    }

    /// 执行一次 `run` 并记录耗时/结果,返回本轮产出。
    /// `Err` 已在记录时计为失败,调用方不再进入 validate。
    async fn once(
        ctx: &S::Ctx,
        task: &TaskIndex,
        setup: &S::Setup,
        recorder: &mut Recorder,
    ) -> Result<S::Output, S::Error> {
        let start = Instant::now();
        #[cfg(feature = "tui")]
        recorder.begin_round();
        let result = S::run(ctx, task, setup).await;
        #[cfg(feature = "tui")]
        recorder.end_round();

        recorder.record_duration(start.elapsed());
        recorder.record_result(&result);
        result
    }

    /// 校验 run 的成功产出;`Err` 同样记为一次 validate 失败。
    async fn validate(
        ctx: &S::Ctx,
        task: &TaskIndex,
        setup: &S::Setup,
        output: &S::Output,
        recorder: &mut Recorder,
    ) {
        let ret = S::validate(ctx, task, setup, output).await;
        match ret {
            Ok(true) => {
                recorder.record_validate(true);
            }
            Ok(false) => {
                recorder.record_validate_failure(Cow::Borrowed("validate"));
            }
            Err(err) => {
                recorder.log_internal(format!("{err:?}"));
                recorder.record_validate_failure(err.kind());
            }
        }
    }
}
