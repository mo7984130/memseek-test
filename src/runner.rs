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

/// 超时退避配置:请求超时后等待 `initial` 起,按 `factor` 指数增长,封顶 `max`;
/// 任意非超时结果(成功或普通失败)复位到 `initial`。
///
/// 目的:被测系统过载时自动减速,避免超时风暴雪上加霜;恢复后自动回到满速。
#[derive(Clone, Copy, Debug)]
pub struct BackoffConfig {
    /// 首次退避等待(默认 100ms)
    pub initial: Duration,
    /// 退避最大值(默认 5s)
    pub max: Duration,
    /// 增长倍数(默认 2.0)
    pub factor: f64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(5),
            factor: 2.0,
        }
    }
}

impl BackoffConfig {
    pub const fn new(initial: Duration, max: Duration, factor: f64) -> Self {
        Self {
            initial,
            max,
            factor,
        }
    }
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
    /// 超时退避;`None` 表示超时后立即进入下一轮(默认)
    pub backoff: Option<BackoffConfig>,
    /// 实时进度上报句柄(feature `tui`,由 Manager 注入);`None` 表示不上报
    #[cfg(feature = "tui")]
    pub progress: Option<Progress>,
}

pub struct ScenarioRunner<S> {
    config: RunnerConfig,
    _marker: PhantomData<S>,
}

/// setup 执行结果。
enum SetupDone<S> {
    /// setup 成功(已复位退避)
    Ready(S),
    /// 普通失败(已记录),任务应中止
    Failed,
    /// 超时退避等待中被停止信号打断,任务应中止
    Cancelled,
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
    /// 返回该轮是否超时(用于触发退避)。
    async fn run_and_validate(
        ctx: &S::Ctx,
        task: &TaskIndex,
        setup: &S::Setup,
        recorder: &mut Recorder,
    ) -> bool {
        let result = Self::once(ctx, task, setup, recorder).await;
        let timed_out = result.as_ref().is_err_and(|e| e.is_timeout());
        // run 失败已记录,不再进入 validate
        if let Ok(output) = &result {
            Self::validate(ctx, task, setup, output, recorder).await;
        }
        timed_out
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
                // 退避状态在 setup 与 run 轮次间共享:任意成功(系统恢复)后复位
                let mut backoff_wait = Self::backoff_initial(self.config.backoff);
                let setup = match Self::setup_retrying(
                    ctx,
                    &setup_task,
                    &mut recorder,
                    self.config.shutdown.as_ref(),
                    self.config.backoff,
                    &mut backoff_wait,
                )
                .await
                {
                    SetupDone::Ready(setup) => setup,
                    SetupDone::Failed => return recorder,
                    SetupDone::Cancelled => {
                        recorder.record_interrupted();
                        return recorder;
                    }
                };
                #[cfg(feature = "tui")]
                recorder.end_setup();
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
                            if Self::run_round(
                                ctx,
                                &task,
                                &setup,
                                &mut recorder,
                                self.config.shutdown.as_ref(),
                                self.config.backoff,
                                &mut backoff_wait,
                            )
                            .await
                            {
                                recorder.record_interrupted();
                                break;
                            }
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

                            if Self::run_round(
                                ctx,
                                &task,
                                &setup,
                                &mut recorder,
                                self.config.shutdown.as_ref(),
                                self.config.backoff,
                                &mut backoff_wait,
                            )
                            .await
                            {
                                recorder.record_interrupted();
                                break;
                            }

                            remaining = remaining.saturating_sub(start.elapsed());
                        }
                    }
                }
            }
            SetupMode::Round => {
                // setup 每轮一次:普通失败中止任务,超时退避重试
                match self.config.mode {
                    RunMode::Times(times) => {
                        let mut backoff_wait = Self::backoff_initial(self.config.backoff);
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
                            let setup = match Self::setup_retrying(
                                ctx,
                                &task,
                                &mut recorder,
                                self.config.shutdown.as_ref(),
                                self.config.backoff,
                                &mut backoff_wait,
                            )
                            .await
                            {
                                SetupDone::Ready(setup) => setup,
                                SetupDone::Failed => return recorder,
                                SetupDone::Cancelled => {
                                    recorder.record_interrupted();
                                    break;
                                }
                            };
                            if Self::run_round(
                                ctx,
                                &task,
                                &setup,
                                &mut recorder,
                                self.config.shutdown.as_ref(),
                                self.config.backoff,
                                &mut backoff_wait,
                            )
                            .await
                            {
                                recorder.record_interrupted();
                                break;
                            }
                        }
                    }
                    RunMode::Duration(duration) => {
                        let mut remaining = duration;
                        let mut round = 0usize;
                        let mut backoff_wait = Self::backoff_initial(self.config.backoff);

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
                            let setup = match Self::setup_retrying(
                                ctx,
                                &task,
                                &mut recorder,
                                self.config.shutdown.as_ref(),
                                self.config.backoff,
                                &mut backoff_wait,
                            )
                            .await
                            {
                                SetupDone::Ready(setup) => setup,
                                SetupDone::Failed => return recorder,
                                SetupDone::Cancelled => {
                                    recorder.record_interrupted();
                                    break;
                                }
                            };

                            if Self::run_round(
                                ctx,
                                &task,
                                &setup,
                                &mut recorder,
                                self.config.shutdown.as_ref(),
                                self.config.backoff,
                                &mut backoff_wait,
                            )
                            .await
                            {
                                recorder.record_interrupted();
                                break;
                            }

                            remaining = remaining.saturating_sub(start.elapsed());
                        }
                    }
                }
            }
        }

        recorder
    }

    /// 退避初始值:未配置退避时用零占位(不产生等待)。
    fn backoff_initial(backoff: Option<BackoffConfig>) -> Duration {
        backoff.map(|b| b.initial).unwrap_or(Duration::ZERO)
    }

    /// 执行 setup,失败时处理同 `run_round` 的退避语义:
    /// 超时 → 退避后重试(可被停止信号打断);普通失败或未配置退避时中止。
    async fn setup_retrying(
        ctx: &S::Ctx,
        task: &TaskIndex,
        recorder: &mut Recorder,
        shutdown: Option<&Shutdown>,
        backoff: Option<BackoffConfig>,
        wait: &mut Duration,
    ) -> SetupDone<S::Setup> {
        let Some(cfg) = backoff else {
            // 未配置退避:保持旧语义,失败即中止
            return match S::setup(ctx, task).await {
                Ok(setup) => SetupDone::Ready(setup),
                Err(err) => {
                    recorder.record_result(&Err::<(), S::Error>(err));
                    SetupDone::Failed
                }
            };
        };
        loop {
            match S::setup(ctx, task).await {
                Ok(setup) => {
                    if *wait != cfg.initial {
                        *wait = cfg.initial;
                    }
                    return SetupDone::Ready(setup);
                }
                Err(err) => {
                    let timed_out = err.is_timeout();
                    recorder.record_result(&Err::<(), S::Error>(err));
                    if !timed_out {
                        return SetupDone::Failed;
                    }
                    if Self::wait_or_cancel(shutdown, *wait).await {
                        return SetupDone::Cancelled;
                    }
                    // 指数增长,封顶 max,不低于 initial
                    let next = wait.as_secs_f64() * cfg.factor;
                    *wait = Duration::from_secs_f64(next.min(cfg.max.as_secs_f64()));
                    *wait = (*wait).max(cfg.initial);
                }
            }
        }
    }

    /// 执行一轮并处理退避:本轮超时则等待退避时间(可被停止信号打断),
    /// 返回 `true` 表示因停止信号提前退出。
    /// 非超时结果(成功或普通失败)将退避复位。
    async fn run_round(
        ctx: &S::Ctx,
        task: &TaskIndex,
        setup: &S::Setup,
        recorder: &mut Recorder,
        shutdown: Option<&Shutdown>,
        backoff: Option<BackoffConfig>,
        wait: &mut Duration,
    ) -> bool {
        let timed_out = Self::run_and_validate(ctx, task, setup, recorder).await;
        let Some(cfg) = backoff else {
            return false;
        };
        if timed_out {
            if Self::wait_or_cancel(shutdown, *wait).await {
                return true;
            }
            // 指数增长,封顶 max,不低于 initial
            let next = wait.as_secs_f64() * cfg.factor;
            *wait = Duration::from_secs_f64(next.min(cfg.max.as_secs_f64()));
            *wait = (*wait).max(cfg.initial);
        } else if *wait != cfg.initial {
            *wait = cfg.initial;
        }
        false
    }

    /// 退避等待;绑定停止信号时,信号到达立即返回 `true`。
    async fn wait_or_cancel(shutdown: Option<&Shutdown>, duration: Duration) -> bool {
        match shutdown {
            Some(s) => tokio::select! {
                _ = tokio::time::sleep(duration) => false,
                _ = s.wait_cancelled() => true,
            },
            None => {
                tokio::time::sleep(duration).await;
                false
            }
        }
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
