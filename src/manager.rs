use std::any::Any;

use futures::future::join_all;

use crate::{
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
                    })
                    .collect()
            }
            RunMode::Duration(d) => (0..concurrency)
                .map(|i| RunnerConfig {
                    mode: RunMode::Duration(d),
                    task_index: i as usize,
                    task_total: concurrency as usize,
                    shutdown: shutdown.cloned(),
                })
                .collect(),
        };

        let futures: Vec<_> = cfgs.iter().map(|c| (entry.invoke)(ctx, c)).collect();
        let results = join_all(futures).await;

        let mut it = results.into_iter();
        let mut merged = it.next().expect("concurrency must be > 0");
        for other in it {
            merged.merge(other);
        }
        ScenarioReport::from_recorder(entry.name, merged, self.config.concurrency)
    }
}
