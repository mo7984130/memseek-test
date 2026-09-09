use std::{any::Any, sync::Arc, sync::atomic::AtomicUsize};

use futures::future::join_all;

use crate::{
    registry::{ScenarioRegistration, ScenarioRegistry},
    report::ScenarioReport,
    runner::{RunMode, RunnerConfig},
};

#[derive(Debug, Clone, Copy)]
pub struct ManagerConfig {
    /// 并发度
    concurrency: u64,
    /// 运行模式, 存在的情况下会覆盖scenario配置
    run_mode: Option<RunMode>,
}
impl ManagerConfig {
    pub fn new(concurrency: u64) -> Self {
        if concurrency == 0 {
            panic!("Manager Config concurrency cannot be zero");
        }
        Self {
            concurrency,
            run_mode: None,
        }
    }

    pub fn with_run_mode(mut self, run_mode: RunMode) -> Self {
        self.run_mode = Some(run_mode);
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

    pub async fn run<Ctx: Any + Sync>(
        &self,
        ctx: &Ctx,
        scenarios: &[&'static ScenarioRegistration],
    ) -> Vec<ScenarioReport> {
        let mut reports = Vec::with_capacity(scenarios.len());
        for s in scenarios {
            reports.push(self.run_entry(s, ctx).await);
        }
        reports
    }

    pub async fn run_one<Ctx: Any + Sync>(&self, name: &str, ctx: &Ctx) -> Option<ScenarioReport> {
        let s = ScenarioRegistry::find::<Ctx>(name)?;
        Some(self.run_entry(s, ctx).await)
    }

    async fn run_entry(&self, entry: &ScenarioRegistration, ctx: &dyn Any) -> ScenarioReport {
        let mode = self
            .config
            .run_mode
            .or(entry.config.run_mode)
            .expect(&format!("{} 未配置执行模式(times/duration)", entry.name));

        let concurrency = self.config.concurrency;
        // 全局运行编号计数器:preset 阶段取号,任务内三阶段共享同一编号
        let round_counter = Arc::new(AtomicUsize::new(0));

        let cfgs: Vec<RunnerConfig> = match mode {
            RunMode::Times(total) => {
                let base = total / concurrency;
                let rem = total % concurrency;
                (0..concurrency)
                    .map(|i| RunnerConfig {
                        mode: RunMode::Times(base + u64::from(i < rem)),
                        task_index: i as usize,
                        task_total: concurrency as usize,
                        round_counter: round_counter.clone(),
                    })
                    .collect()
            }
            RunMode::Duration(d) => (0..concurrency)
                .map(|i| RunnerConfig {
                    mode: RunMode::Duration(d),
                    task_index: i as usize,
                    task_total: concurrency as usize,
                    round_counter: round_counter.clone(),
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
