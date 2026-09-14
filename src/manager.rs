use std::any::Any;

use futures::future::join_all;

#[cfg(feature = "tui")]
use crate::tui::TuiOptions;
use crate::{
    progress::{Progress, ProgressOptions, ProgressPlan},
    registry::{ScenarioRegistration, ScenarioRegistry},
    report::ScenarioReport,
    runner::{RunMode, RunnerConfig},
    shutdown::Shutdown,
};

#[derive(Debug)]
pub struct ManagerConfig {
    /// 并发度
    concurrency: u64,
    /// 运行模式, 存在的情况下会覆盖scenario配置
    run_mode: Option<RunMode>,
    /// 全局优雅关闭信号;也可在调用时用 `run*_with_shutdown` 显式传入
    shutdown: Option<Shutdown>,
    /// 实时进度显示选项;`None` 表示关闭(默认)
    progress: Option<ProgressOptions>,
    /// 终端 TUI 显示选项(feature `tui`);与 `progress` 互斥
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
            run_mode: None,
            shutdown: None,
            progress: None,
            #[cfg(feature = "tui")]
            tui: None,
        }
    }

    pub fn with_run_mode(mut self, run_mode: RunMode) -> Self {
        self.run_mode = Some(run_mode);
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

    /// 开启实时进度显示(单行,输出到 stderr)。
    ///
    /// 每个场景一行:`场景名 [进度条] 百分比 已完成/计划 吞吐 [fail] [inflight]`;
    /// 场景结束(含优雅关闭)后该行定稿保留。stderr 非 TTY(CI/重定向)时
    /// 自动静默,可用 [`Self::with_progress_options`] 配合
    /// [`ProgressOptions::force`] 强制输出。
    pub fn with_progress(mut self) -> Self {
        self.progress = Some(ProgressOptions::default());
        self
    }

    /// 同 [`Self::with_progress`],但自定义刷新间隔/颜色/条宽等选项。
    pub fn with_progress_options(mut self, options: ProgressOptions) -> Self {
        self.progress = Some(options);
        self
    }

    /// 开启终端 TUI 模式(需 feature `tui`):全屏显示顶部进度 + 日志区,
    /// 退出后日志缓冲回放到 stderr。与 [`Self::with_progress`] 互斥。
    /// 非 TTY(CI/重定向)时自动静默。
    #[cfg(feature = "tui")]
    pub fn with_tui(mut self) -> Self {
        self.tui = Some(TuiOptions::default());
        self
    }

    /// 同 [`Self::with_tui`],自定义日志区行数/缓冲/刷新等选项。
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
        for s in scenarios {
            // 优雅关闭:信号已触发则不再启动下一个场景
            if shutdown.is_some_and(|s| s.is_cancelled()) {
                break;
            }
            reports.push(self.run_entry(s, ctx, shutdown).await);
        }
        reports
    }

    pub async fn run_one<Ctx: Any + Sync>(&self, name: &str, ctx: &Ctx) -> Option<ScenarioReport> {
        let s = ScenarioRegistry::find::<Ctx>(name)?;
        Some(self.run_entry(s, ctx, self.config.shutdown.as_ref()).await)
    }

    /// 同 [`Self::run_one`],但以显式传入的关闭信号替代 `ManagerConfig` 中绑定的信号。
    pub async fn run_one_with_shutdown<Ctx: Any + Sync>(
        &self,
        name: &str,
        ctx: &Ctx,
        shutdown: Shutdown,
    ) -> Option<ScenarioReport> {
        let s = ScenarioRegistry::find::<Ctx>(name)?;
        Some(self.run_entry(s, ctx, Some(&shutdown)).await)
    }

    async fn run_entry(
        &self,
        entry: &ScenarioRegistration,
        ctx: &dyn Any,
        shutdown: Option<&Shutdown>,
    ) -> ScenarioReport {
        let mode = self
            .config
            .run_mode
            .or(entry.config.run_mode)
            .unwrap_or_else(|| panic!("{} 未配置执行模式(times/duration)", entry.name));

        let concurrency = self.config.concurrency;

        let plan = match mode {
            RunMode::Times(total) => ProgressPlan::Rounds(total),
            RunMode::Duration(d) => ProgressPlan::Time(d),
        };

        // 进度显示模式二选一:单行(progress)或 TUI(feature `tui`);
        // 两种模式共享同一进度计数,日志通道由 TUI 持有。
        #[cfg(feature = "tui")]
        let tui = self
            .config
            .tui
            .clone()
            .map(|mut options| (options.clone(), options.take_channel()));

        let progress = self
            .config
            .progress
            .map(|options| Progress::new(entry.name, plan, options, None));

        #[cfg(feature = "tui")]
        let progress = match tui.as_ref() {
            Some((_options, channel)) => {
                if progress.is_some() {
                    panic!("with_progress 与 with_tui 互斥,请只启用其一");
                }
                Some(Progress::new(
                    entry.name,
                    plan,
                    ProgressOptions::default(),
                    Some(channel.clone()),
                ))
            }
            None => progress,
        };

        // 渲染任务:单行或 TUI;未开启时为 None,零开销
        #[cfg(feature = "tui")]
        let progress_guard = match tui.as_ref() {
            Some((options, channel)) => progress
                .as_ref()
                .map(|p| p.start_tui(channel.clone(), options.clone(), shutdown.cloned())),
            None => progress.as_ref().map(|p| p.start(shutdown.cloned())),
        };
        #[cfg(not(feature = "tui"))]
        let progress_guard = progress.as_ref().map(|p| p.start(shutdown.cloned()));

        let cfgs: Vec<RunnerConfig> = match mode {
            RunMode::Times(total) => {
                let base = total / concurrency;
                let rem = total % concurrency;
                (0..concurrency)
                    .map(|i| RunnerConfig {
                        mode: RunMode::Times(base + u64::from(i < rem)),
                        task_index: i as usize,
                        task_total: concurrency as usize,
                        shutdown: shutdown.cloned(),
                        progress: progress.clone(),
                    })
                    .collect()
            }
            RunMode::Duration(d) => (0..concurrency)
                .map(|i| RunnerConfig {
                    mode: RunMode::Duration(d),
                    task_index: i as usize,
                    task_total: concurrency as usize,
                    shutdown: shutdown.cloned(),
                    progress: progress.clone(),
                })
                .collect(),
        };

        let futures: Vec<_> = cfgs.iter().map(|c| (entry.invoke)(ctx, c)).collect();
        let results = join_all(futures).await;

        // 所有轮次结束后停止渲染,等最后一行写完,再交给调用方打印报告;
        // 提前返回/取消的路径由守卫 drop 兜底停止。
        if let Some(guard) = progress_guard {
            guard.stop().await;
        }

        // TUI:渲染已恢复原屏幕,把日志缓冲回放到 stderr 补全档案
        #[cfg(feature = "tui")]
        if let Some((_, channel)) = tui.as_ref() {
            for line in channel.drain() {
                eprintln!("{line}");
            }
        }

        let mut it = results.into_iter();
        let mut merged = it.next().expect("concurrency must be > 0");
        for other in it {
            merged.merge(other);
        }
        ScenarioReport::from_recorder(entry.name, merged, self.config.concurrency)
    }
}
