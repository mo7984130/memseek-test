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
    scenario::{Scenario, SetupMode, TeardownMode},
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
            max: Duration::from_secs(3),
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

/// 轮次预算:`Times` 按次数消耗,`Duration` 按墙钟消耗。
///
/// 两种执行模式共用同一套轮次循环骨架(取消检查 → 轮级 setup →
/// 轮体 run/validate/teardown → 退避),预算只决定"是否还有下一轮"。
#[derive(Clone, Copy, Debug)]
enum RoundBudget {
    Times {
        left: u64,
    },
    /// 剩余时长,每轮按该轮实际耗时(含 setup 与 teardown)扣减
    Duration {
        remaining: Duration,
    },
}

impl RoundBudget {
    const fn new(mode: RunMode) -> Self {
        match mode {
            RunMode::Times(times) => Self::Times { left: times },
            RunMode::Duration(duration) => Self::Duration {
                remaining: duration,
            },
        }
    }

    /// 是否还有预算执行下一轮
    fn has_next(&self) -> bool {
        match self {
            Self::Times { left } => *left > 0,
            Self::Duration { remaining } => !remaining.is_zero(),
        }
    }

    /// 记一轮的消耗
    fn spend(&mut self, elapsed: Duration) {
        match self {
            Self::Times { left } => *left = left.saturating_sub(1),
            Self::Duration { remaining } => *remaining = remaining.saturating_sub(elapsed),
        }
    }
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

        // 优雅关闭:信号到达时已完成当前轮次,不再启动新轮次。
        // 各分支在 setup 后/每轮开始前检查。
        // 退避状态在 setup 与各轮之间共享:任意非超时结果(系统恢复)后复位。
        let mut backoff_wait = Self::backoff_initial(self.config.backoff);

        match S::SETUP_MODE {
            SetupMode::Task => {
                if self.is_cancelled() {
                    recorder.record_interrupted();
                    return recorder;
                }
                // setup 任务级一次(round 无轮次含义,固定 0),产出供各轮复用
                let task = TaskIndex::new(self.config.task_index, self.config.task_total, 0);
                #[cfg(feature = "tui")]
                recorder.begin_setup();
                let setup = match self
                    .setup_retrying(ctx, &task, &mut recorder, &mut backoff_wait)
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
                } else {
                    self.drive_rounds(ctx, &setup, &mut recorder, &mut backoff_wait)
                        .await;
                }
                // 任务级收尾:finally 语义(取消后也执行);setup 成功过才会走到这里
                if S::TEARDOWN_MODE == TeardownMode::Task {
                    Self::teardown(ctx, &task, &setup, None, &mut recorder).await;
                }
            }
            SetupMode::Round => {
                let mut budget = RoundBudget::new(self.config.mode);
                let mut round = 0usize;
                // 轮级 setup 的产出仅本轮有效;任务级收尾(TEARDOWN_MODE = Task)
                // 需要一个 setup 借用,这里保留最近一次成功的产出供其使用。
                let mut last: Option<(TaskIndex, S::Setup)> = None;

                while budget.has_next() {
                    if self.is_cancelled() {
                        recorder.record_interrupted();
                        break;
                    }
                    let start = Instant::now();
                    let task =
                        TaskIndex::new(self.config.task_index, self.config.task_total, round);
                    round += 1;

                    let setup = match self
                        .setup_retrying(ctx, &task, &mut recorder, &mut backoff_wait)
                        .await
                    {
                        SetupDone::Ready(setup) => setup,
                        // 普通失败:中止任务(不再启动新轮次)
                        SetupDone::Failed => break,
                        SetupDone::Cancelled => {
                            recorder.record_interrupted();
                            break;
                        }
                    };

                    let entry = (task, setup);
                    let stopped = self
                        .round(ctx, &entry.0, &entry.1, &mut recorder, &mut backoff_wait)
                        .await;
                    budget.spend(start.elapsed());
                    last = Some(entry);
                    if stopped {
                        recorder.record_interrupted();
                        break;
                    }
                }

                if S::TEARDOWN_MODE == TeardownMode::Task
                    && let Some((task, setup)) = last.as_ref()
                {
                    Self::teardown(ctx, task, setup, None, &mut recorder).await;
                }
            }
        }

        recorder
    }

    /// `SetupMode::Task` 下的轮次循环:每轮复用同一份 setup 产出。
    async fn drive_rounds(
        &self,
        ctx: &S::Ctx,
        setup: &S::Setup,
        recorder: &mut Recorder,
        wait: &mut Duration,
    ) {
        let mut budget = RoundBudget::new(self.config.mode);
        let mut round = 0usize;

        while budget.has_next() {
            if self.is_cancelled() {
                recorder.record_interrupted();
                return;
            }
            let task = TaskIndex::new(self.config.task_index, self.config.task_total, round);
            round += 1;

            let start = Instant::now();
            if self.round(ctx, &task, setup, recorder, wait).await {
                recorder.record_interrupted();
                return;
            }
            budget.spend(start.elapsed());
        }
    }

    /// 退避初始值:未配置退避时用零占位(不产生等待)。
    fn backoff_initial(backoff: Option<BackoffConfig>) -> Duration {
        backoff.map(|b| b.initial).unwrap_or(Duration::ZERO)
    }

    /// 执行 setup,失败时处理同轮体的退避语义:
    /// 超时 → 退避后重试(可被停止信号打断);普通失败或未配置退避时中止。
    /// 每次尝试的耗时计入 setup 统计。
    async fn setup_retrying(
        &self,
        ctx: &S::Ctx,
        task: &TaskIndex,
        recorder: &mut Recorder,
        wait: &mut Duration,
    ) -> SetupDone<S::Setup> {
        let Some(cfg) = self.config.backoff else {
            // 未配置退避:保持旧语义,失败即中止
            let start = Instant::now();
            let ret = S::setup(ctx, task).await;
            recorder.record_setup_duration(start.elapsed());
            return match ret {
                Ok(setup) => SetupDone::Ready(setup),
                Err(err) => {
                    recorder.record_result(&Err::<(), S::Error>(err));
                    SetupDone::Failed
                }
            };
        };
        loop {
            let start = Instant::now();
            let ret = S::setup(ctx, task).await;
            recorder.record_setup_duration(start.elapsed());
            match ret {
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
                    if self.wait_or_cancel(*wait).await {
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

    /// 执行一轮:`run → validate(仅成功时) → teardown(finally)`。
    /// 返回 `true` 表示本轮超时后的退避等待被停止信号打断。
    ///
    /// 退避只由 `run` 的超时驱动:`teardown` 的耗时与失败都不参与退避。
    async fn round(
        &self,
        ctx: &S::Ctx,
        task: &TaskIndex,
        setup: &S::Setup,
        recorder: &mut Recorder,
        wait: &mut Duration,
    ) -> bool {
        let result = Self::once(ctx, task, setup, recorder).await;
        let timed_out = result.as_ref().is_err_and(|e| e.is_timeout());
        // run 失败已记录,不再进入 validate
        if let Ok(output) = &result {
            Self::validate(ctx, task, setup, output, recorder).await;
        }
        // 轮级收尾:finally 语义——run/validate 无论成败都会执行
        if S::TEARDOWN_MODE == TeardownMode::Round {
            Self::teardown(ctx, task, setup, Some(result.as_ref()), recorder).await;
        }

        let Some(cfg) = self.config.backoff else {
            return false;
        };
        if timed_out {
            if self.wait_or_cancel(*wait).await {
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
    async fn wait_or_cancel(&self, duration: Duration) -> bool {
        match self.config.shutdown.as_ref() {
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
        let start = Instant::now();
        let ret = S::validate(ctx, task, setup, output).await;
        recorder.record_validate_duration(start.elapsed());
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

    /// 收尾阶段:记录耗时与结果。
    ///
    /// finally 语义——`run`/`validate` 无论成败都会调用,且不可被停止信号打断。
    /// 失败只记账(`teardown_failures` + `teardown:{kind}` 错误明细),
    /// 不加剧 `failures`/`timeouts`,不触发退避,也不中止任务。
    /// `result` 为该轮 `run` 的结果(`TEARDOWN_MODE = Round`);任务级收尾传 `None`。
    async fn teardown(
        ctx: &S::Ctx,
        task: &TaskIndex,
        setup: &S::Setup,
        result: Option<Result<&S::Output, &S::Error>>,
        recorder: &mut Recorder,
    ) {
        let start = Instant::now();
        let ret = S::teardown(ctx, task, setup, result).await;
        recorder.record_teardown_duration(start.elapsed());
        match ret {
            Ok(()) => recorder.record_teardown(true),
            Err(err) => {
                recorder.log_internal(format!("{err:?}"));
                recorder.record_teardown(false);
                recorder.record_teardown_error(err.kind());
            }
        }
    }
}
