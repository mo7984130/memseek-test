use futures::future::join_all;
use std::marker::PhantomData;

use crate::{
    recorders::recorder::ScenarioRecorder,
    report::ScenarioReport,
    runner::{RunnerConfig, ScenarioRunner},
    scenario::Scenario,
};

pub struct ManagerConfig {
    concurrency: u64,
    total: u64,
}
impl ManagerConfig {
    pub fn new(total: u64, concurrency: u64) -> Self {
        if total == 0 {
            panic!("Manager Config total cannot be zero");
        }
        if concurrency == 0 {
            panic!("Manager Config concurrency cannot be zero");
        }
        Self { concurrency, total }
    }
}

pub struct ScenarioManager<S, R> {
    config: ManagerConfig,
    _marker1: PhantomData<S>,
    _marker2: PhantomData<R>,
}

impl<S, R> ScenarioManager<S, R>
where
    S: Scenario,
    R: ScenarioRecorder,
{
    pub fn new(config: ManagerConfig) -> Self {
        Self {
            config,
            _marker1: PhantomData,
            _marker2: PhantomData,
        }
    }

    pub async fn run(&self, ctx: &S::Ctx) -> ScenarioReport {
        let cfg = &self.config;

        let concurrency = cfg.concurrency.min(cfg.total);

        let base = cfg.total / concurrency;
        let remainder = cfg.total % concurrency;

        let futures = (0..concurrency).map(|i| {
            let once = base + u64::from(i < remainder);

            let mut runner = ScenarioRunner::<R>::new(RunnerConfig { times: once });
            async move { runner.run::<S>(&ctx).await }
        });

        let mut results = join_all(futures).await;

        let mut result = results.pop().expect("concurrency must be greater than 0");

        for other in results {
            result.merge(other);
        }

        ScenarioReport::from_recorder(S::name(), result)
    }
}
