use std::{any::Any, panic, time::Instant};

use futures::future::join_all;
use tokio::runtime::RuntimeFlavor;
use tracing::warn;

#[cfg(feature = "tui")]
use crate::progress::{Progress, ProgressGuard, ProgressPlan};
#[cfg(feature = "tui")]
use crate::tui::TuiOptions;
use crate::{
    recorder::Recorder,
    registry::{ScenarioInvoke, ScenarioRegistration, ScenarioRegistry},
    report::ScenarioReport,
    runner::{BackoffConfig, RunMode, RunnerConfig},
    shutdown::Shutdown,
};

#[derive(Debug)]
pub struct ManagerConfig {
    /// 并发度
    concurrency: u64,
    /// 驱动并发任务的线程数(分片数),默认 1
    workers: u64,
    /// 运行模式, 存在的情况下会覆盖scenario配置
    run_mode: Option<RunMode>,
    /// 超时退避;`None` 表示超时后立即进入下一轮(默认)
    backoff: Option<BackoffConfig>,
    /// 全局优雅关闭信号;也可在调用时用 `run*_with_shutdown` 显式传入
    shutdown: Option<Shutdown>,
    /// 终端 TUI 显示选项(feature `tui`);`None` 表示关闭(默认)
    #[cfg(feature = "tui")]
    tui: Option<TuiOptions>,
}
impl ManagerConfig {
    pub fn new(concurrency: u64) -> Self {
        if concurrency == 0 {
            panic!("Manager Config concurrency cannot be zero");
        }
        Self {
            concurrency,
            workers: 1,
            run_mode: None,
            backoff: None,
            shutdown: None,
            #[cfg(feature = "tui")]
            tui: None,
        }
    }

    /// 用多个线程驱动并发任务(默认 1 个,即在当前 runtime 上协作调度)。
    ///
    /// `workers > 1` 时,并发任务按连续区间分片到等量的框架自建线程,
    /// 每线程一个 `current_thread` runtime;分片内仍按 `join_all` 协作调度。
    /// 目的:让 CPU 密集的任务真正并行,避免单线程的自伤延迟。
    ///
    /// 只改变"由哪个线程驱动",不改变施加的并发:`task_index`/`task_total`
    /// 仍为全局编号,报告口径与 RPS 计算不变;`workers` 超过并发度时按并发度裁剪。
    ///
    /// 注意:分片执行会阻塞调用线程(等价于 `block_on` 的语义),应由
    /// `block_on` 的顶层调用;若调用方 runtime 是 `current_thread`,
    /// 分片期间已 spawn 的后台任务(TUI 渲染、Ctrl-C 监听)将得不到调度,
    /// 此时请改用多线程 runtime 或保持 `workers = 1`(会给出告警)。
    pub fn with_workers(mut self, workers: u64) -> Self {
        if workers == 0 {
            panic!("Manager Config workers cannot be zero");
        }
        self.workers = workers;
        self
    }

    pub fn with_run_mode(mut self, run_mode: RunMode) -> Self {
        self.run_mode = Some(run_mode);
        self
    }

    /// 启用超时退避:请求超时(过载信号)后等待退避时间再发下一轮,
    /// 指数增长封顶 `max`,请求恢复成功后复位。
    /// 未启用时超时后立即进入下一轮。
    pub fn with_backoff(mut self, backoff: BackoffConfig) -> Self {
        self.backoff = Some(backoff);
        self
    }

    /// 绑定优雅关闭信号:触发后各并发任务完成当前轮次即停止,
    /// 未开始执行的场景不再执行。
    pub fn with_shutdown(mut self, shutdown: Shutdown) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    /// 便捷绑定:监听 OS 的 Ctrl-C(SIGINT),收到信号即触发优雅关闭。
    /// 须在 Tokio runtime 内调用。
    pub fn install_ctrl_c(mut self) -> Self {
        self.shutdown = Some(Shutdown::install_ctrl_c());
        self
    }

    /// 开启终端 TUI 模式(需 feature `tui`):进 alternate screen 全屏显示
    /// 顶部进度行 + 日志滚动区,退出后日志缓冲回放到 stderr 补全档案。
    /// 非 TTY(CI/重定向)时自动静默(不做任何输出)。
    #[cfg(feature = "tui")]
    pub fn with_tui(mut self) -> Self {
        self.tui = Some(TuiOptions::default());
        self
    }

    /// 同 [`Self::with_tui`],自定义日志区行数/缓冲/刷新/颜色等选项。
    #[cfg(feature = "tui")]
    pub fn with_tui_options(mut self, options: TuiOptions) -> Self {
        self.tui = Some(options);
        self
    }
}

pub struct ScenarioManager {
    config: ManagerConfig,
}

impl ScenarioManager {
    pub fn new(config: ManagerConfig) -> Self {
        Self { config }
    }

    pub async fn run_all<Ctx: Any + Sync>(&self, ctx: &Ctx) -> Vec<ScenarioReport> {
        let scenarios = ScenarioRegistry::scenarios::<Ctx>();
        self.run(ctx, &scenarios).await
    }

    /// 同 [`Self::run_all`],但以显式传入的关闭信号替代 `ManagerConfig` 中绑定的信号。
    pub async fn run_all_with_shutdown<Ctx: Any + Sync>(
        &self,
        ctx: &Ctx,
        shutdown: Shutdown,
    ) -> Vec<ScenarioReport> {
        let scenarios = ScenarioRegistry::scenarios::<Ctx>();
        self.run_with_shutdown(ctx, &scenarios, shutdown).await
    }

    pub async fn run<Ctx: Any + Sync>(
        &self,
        ctx: &Ctx,
        scenarios: &[&'static ScenarioRegistration],
    ) -> Vec<ScenarioReport> {
        self.run_inner(ctx, scenarios, self.config.shutdown.as_ref())
            .await
    }

    /// 同 [`Self::run`],但以显式传入的关闭信号替代 `ManagerConfig` 中绑定的信号。
    pub async fn run_with_shutdown<Ctx: Any + Sync>(
        &self,
        ctx: &Ctx,
        scenarios: &[&'static ScenarioRegistration],
        shutdown: Shutdown,
    ) -> Vec<ScenarioReport> {
        self.run_inner(ctx, scenarios, Some(&shutdown)).await
    }

    async fn run_inner<Ctx: Any + Sync>(
        &self,
        ctx: &Ctx,
        scenarios: &[&'static ScenarioRegistration],
        shutdown: Option<&Shutdown>,
    ) -> Vec<ScenarioReport> {
        let mut reports = Vec::with_capacity(scenarios.len());
        for (i, s) in scenarios.iter().enumerate() {
            // 优雅关闭:信号已触发则不再启动下一个场景
            if shutdown.is_some_and(|s| s.is_cancelled()) {
                break;
            }
            reports.push(self.run_entry(s, ctx, shutdown, i, scenarios.len()).await);
        }
        reports
    }

    pub async fn run_one<Ctx: Any + Sync>(&self, name: &str, ctx: &Ctx) -> Option<ScenarioReport> {
        let s = ScenarioRegistry::find::<Ctx>(name)?;
        Some(
            self.run_entry(s, ctx, self.config.shutdown.as_ref(), 0, 1)
                .await,
        )
    }

    /// 同 [`Self::run_one`],但以显式传入的关闭信号替代 `ManagerConfig` 中绑定的信号。
    pub async fn run_one_with_shutdown<Ctx: Any + Sync>(
        &self,
        name: &str,
        ctx: &Ctx,
        shutdown: Shutdown,
    ) -> Option<ScenarioReport> {
        let s = ScenarioRegistry::find::<Ctx>(name)?;
        Some(self.run_entry(s, ctx, Some(&shutdown), 0, 1).await)
    }

    async fn run_entry(
        &self,
        entry: &ScenarioRegistration,
        // 需 `Sync` 才能 `&Ctx` 跨线程分片(见 `run_tasks`)
        ctx: &(dyn Any + Sync),
        shutdown: Option<&Shutdown>,
        scenario_index: usize,
        scenario_total: usize,
    ) -> ScenarioReport {
        // 非 TUI 编译时这两个参数仅用于进度显示,此处消音
        #[cfg(not(feature = "tui"))]
        let _ = (scenario_index, scenario_total);

        let mode = self
            .config
            .run_mode
            .or(entry.config.run_mode)
            .unwrap_or_else(|| panic!("{} 未配置执行模式(times/duration)", entry.name));

        let concurrency = self.config.concurrency;

        // TUI 进度显示(feature `tui`):日志通道由配置注入或自动创建。
        // 未开启 feature 或未配置时,进度为 None,零开销。
        #[cfg(feature = "tui")]
        let tui = self
            .config
            .tui
            .clone()
            .map(|mut options| (options.clone(), options.take_channel()));

        #[cfg(feature = "tui")]
        let progress: Option<Progress> = tui.as_ref().map(|(_options, channel)| {
            let plan = match mode {
                RunMode::Times(total) => ProgressPlan::Rounds(total),
                RunMode::Duration(d) => ProgressPlan::Time(d),
            };
            Progress::new(
                entry.name,
                plan,
                Some(channel.clone()),
                scenario_index,
                scenario_total,
            )
        });

        #[cfg(feature = "tui")]
        let progress_guard: Option<ProgressGuard> = match tui.as_ref() {
            Some((options, channel)) => progress
                .as_ref()
                .map(|p| p.start_tui(channel.clone(), options.clone(), shutdown.cloned())),
            None => None,
        };

        let mk_config = |i: u64, mode: RunMode| RunnerConfig {
            mode,
            task_index: i as usize,
            task_total: concurrency as usize,
            shutdown: shutdown.cloned(),
            backoff: self.config.backoff,
            #[cfg(feature = "tui")]
            progress: progress.clone(),
        };

        let cfgs: Vec<RunnerConfig> = match mode {
            RunMode::Times(total) => {
                let base = total / concurrency;
                let rem = total % concurrency;
                (0..concurrency)
                    .map(|i| mk_config(i, RunMode::Times(base + u64::from(i < rem))))
                    .collect()
            }
            RunMode::Duration(d) => (0..concurrency)
                .map(|i| mk_config(i, RunMode::Duration(d)))
                .collect(),
        };

        // 墙钟耗时:整个场景从分发到全部任务结束,用于报告 RPS
        let start = Instant::now();
        let results = run_tasks(ctx, entry.invoke, cfgs, self.config.workers).await;
        let elapsed = start.elapsed();

        // 所有轮次结束后停止渲染,等渲染任务收尾(恢复屏幕),
        // 再交给调用方打印报告;提前返回/取消的路径由守卫 drop 兜底。
        #[cfg(feature = "tui")]
        if let Some(guard) = progress_guard {
            guard.stop().await;
        }

        // TUI:渲染已恢复原屏幕;收到过停止信号时先给醒目提示,
        // 再把日志缓冲回放到 stderr 补全档案
        #[cfg(feature = "tui")]
        if let Some((_, channel)) = tui.as_ref() {
            if shutdown.is_some_and(|s| s.is_cancelled()) {
                eprintln!(
                    "{}",
                    crate::report::paint(
                        "^C received, graceful shutdown: finishing current rounds, report below",
                        "1;33",
                        true,
                    )
                );
            }
            for line in channel.drain() {
                eprintln!("{line}");
            }
        }

        let mut it = results.into_iter();
        let mut merged = it.next().expect("concurrency must be > 0");
        for other in it {
            merged.merge(other);
        }
        ScenarioReport::from_recorder(entry.name, merged, self.config.concurrency, elapsed)
    }
}

/// 分发并发任务并汇总结果。
///
/// `workers == 1`(默认)在当前 runtime 上以 `join_all` 协作调度;`workers > 1`
/// 时按连续区间分片到等量线程,每线程一个 `current_thread` runtime 真正并行
/// (分片内仍协作调度)。分片只决定驱动线程:`task_index`/`task_total` 保持全局
/// 编号,因此参数化与统计口径不变。
async fn run_tasks(
    ctx: &(dyn Any + Sync),
    invoke: ScenarioInvoke,
    cfgs: Vec<RunnerConfig>,
    workers: u64,
) -> Vec<Recorder> {
    let workers = (workers.max(1) as usize).min(cfgs.len().max(1));
    if workers <= 1 {
        let futures: Vec<_> = cfgs.iter().map(|c| invoke(ctx, c)).collect();
        return join_all(futures).await;
    }

    // 分片会阻塞调用线程:current_thread runtime 下已 spawn 的后台任务
    // (TUI 渲染、Ctrl-C 监听)会饿死,这里给出显式提示。
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|h| matches!(h.runtime_flavor(), RuntimeFlavor::CurrentThread))
    {
        warn!(
            "workers = {workers} blocks the caller thread for the whole scenario; \
             on a current_thread runtime, spawned tasks (TUI rendering, Ctrl-C listener) \
             will starve — use a multi-thread runtime or keep workers = 1"
        );
    }

    // 连续分片:前 `rem` 个分片各多担一个任务
    let base = cfgs.len() / workers;
    let rem = cfgs.len() % workers;
    let mut shards: Vec<Vec<RunnerConfig>> = Vec::with_capacity(workers);
    let mut rest = cfgs.into_iter();
    for k in 0..workers {
        let n = base + usize::from(k < rem);
        shards.push(rest.by_ref().take(n).collect());
    }

    std::thread::scope(|scope| {
        let handles: Vec<_> = shards
            .into_iter()
            .map(|shard| {
                scope.spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("build workers runtime failed");
                    let futures: Vec<_> = shard.iter().map(|c| invoke(ctx, c)).collect();
                    rt.block_on(join_all(futures))
                })
            })
            .collect();

        // 即便某个分片 panic,也先等其余分片收尾,再把 panic 原样抛给调用方
        let mut results = Vec::with_capacity(handles.len());
        let mut panicked = None;
        for handle in handles {
            match handle.join() {
                Ok(recorders) => results.push(recorders),
                Err(payload) => {
                    if panicked.is_none() {
                        panicked = Some(payload);
                    }
                }
            }
        }
        if let Some(payload) = panicked {
            panic::resume_unwind(payload);
        }
        results.into_iter().flatten().collect()
    })
}
