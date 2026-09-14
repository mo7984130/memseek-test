use std::{
    any::{Any, TypeId},
    future::Future,
    pin::Pin,
};

use crate::{
    error::ScenarioError,
    recorder::Recorder,
    runner::{RunMode, RunnerConfig, ScenarioRunner},
    scenario::Scenario,
};

/// `None` 表示跟随 Manager 全局默认模式。并发由 Manager 统一管理。
#[derive(Clone, Copy, Debug)]
pub struct ScenarioConfig {
    pub run_mode: Option<RunMode>,
}

impl ScenarioConfig {
    pub const fn new(run_mode: Option<RunMode>) -> Self {
        Self { run_mode }
    }
}

/// 场景统一入口的函数指针类型:接收 Ctx 与配置,返回 runner 的结果。
///
/// 抽成别名以降低 [`ScenarioRegistration::invoke`] 字段的类型复杂度;
/// 宏 `register_scenario!` 生成的条目以 `invoke::<S>` 函数项指针存入。
pub type ScenarioInvoke =
    for<'a> fn(&'a dyn Any, &'a RunnerConfig) -> Pin<Box<dyn Future<Output = Recorder> + 'a>>;

pub struct ScenarioRegistration {
    /// 场景名(用于 `find` / `run_one` 匹配)。
    pub name: &'static str,
    /// 返回该场景 `Ctx` 的 TypeId;发现时按 Context 过滤。
    pub ctx_type: fn() -> TypeId,
    /// 场景默认配置(宏里指定;运行时被 Manager 全局配置覆盖)。
    pub config: ScenarioConfig,
    /// 统一入口(宏生成,函数项指针 `invoke::<S>`)。
    pub invoke: ScenarioInvoke,
}

inventory::collect!(ScenarioRegistration);

/// 统一入口的实现:downcast Ctx + 委托 `ScenarioRunner`,返回 runner 的结果。
/// 宏 `scenario!` 生成的条目以 `invoke::<S>` 函数项指针保存。
pub fn invoke<'a, S>(
    ctx: &'a dyn Any,
    cfg: &'a RunnerConfig,
) -> Pin<Box<dyn Future<Output = Recorder> + 'a>>
where
    S: Scenario,
    S::Error: ScenarioError,
{
    let ctx = ctx.downcast_ref::<S::Ctx>().expect("Ctx 类型不匹配");
    Box::pin(async move {
        let runner = ScenarioRunner::<S>::new(RunnerConfig {
            mode: cfg.mode,
            task_index: cfg.task_index,
            task_total: cfg.task_total,
            shutdown: cfg.shutdown.clone(),
            #[cfg(feature = "tui")]
            progress: cfg.progress.clone(),
        });
        runner.run(ctx).await
    })
}

pub struct ScenarioRegistry;
impl ScenarioRegistry {
    pub fn scenarios<Ctx: Any + Sync>() -> Vec<&'static ScenarioRegistration> {
        let id = TypeId::of::<Ctx>();
        inventory::iter::<ScenarioRegistration>
            .into_iter()
            .filter(|e| (e.ctx_type)() == id)
            .collect()
    }

    pub fn names<Ctx: Any + Sync>() -> Vec<&'static str> {
        Self::scenarios::<Ctx>()
            .into_iter()
            .map(|e| e.name)
            .collect()
    }

    pub fn find<Ctx: Any + Sync>(name: &str) -> Option<&'static ScenarioRegistration> {
        Self::scenarios::<Ctx>()
            .into_iter()
            .find(|e| e.name == name)
    }
}
