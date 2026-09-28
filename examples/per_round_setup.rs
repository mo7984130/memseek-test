//! 按轮的 `setup` / `teardown`:每轮准备一份输入,结束清掉本轮产物。
//!
//! 演示三件事:
//! 1. `SETUP_MODE` / `TEARDOWN_MODE` 设为 `Round` 时,准备与清理按轮执行;
//! 2. `teardown` 是 finally 语义 —— `run` 成功失败都会进来;
//! 3. teardown 失败被单列(`teardown_failures`),不影响通过率,也不触发退避。
//!
//! ```text
//! cargo run --example per_round_setup
//! ```

mod shared;

use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use memseek_test::{
    Report, RunMode, TaskIndex,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    scenario::{Scenario, SetupMode, TeardownMode},
};

use shared::DemoError;

/// 上下文里的「服务端存储」:记录当前还存活着的临时资源。
struct Ctx {
    alive: Mutex<Vec<u64>>,
    next_id: AtomicU64,
}

/// 每轮的产物:本轮新建的资源 id。
struct Upload {
    id: u64,
}

impl Default for Upload {
    fn default() -> Self {
        Self { id: 0 }
    }
}

#[derive(Default)]
struct UploadScenario;

impl Scenario for UploadScenario {
    type Ctx = Ctx;
    type Error = DemoError;
    type Output = u64;
    type Setup = Upload;

    /// 每轮都要一份新的输入 -> 准备按轮
    const SETUP_MODE: SetupMode = SetupMode::Round;
    /// 每轮结束后都要清掉本轮产物 -> 清理按轮
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Round;

    async fn setup(ctx: &Ctx, _task: &TaskIndex) -> Result<Upload, DemoError> {
        let id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
        ctx.alive.lock().unwrap().push(id);
        Ok(Upload { id })
    }

    async fn run(_ctx: &Ctx, _task: &TaskIndex, setup: &Upload) -> Result<u64, DemoError> {
        Ok(setup.id)
    }

    async fn validate(
        _ctx: &Ctx,
        _task: &TaskIndex,
        setup: &Upload,
        out: &u64,
    ) -> Result<bool, DemoError> {
        Ok(*out == setup.id)
    }

    /// finally 语义:无论 run 成功失败都会执行。
    /// 幂等很重要 —— 资源已经不存在应当视为清理成功,而不是收尾失败。
    async fn teardown(
        ctx: &Ctx,
        _task: &TaskIndex,
        _setup: &Upload,
        result: Option<Result<&u64, &DemoError>>,
    ) -> Result<(), DemoError> {
        let Some(Ok(id)) = result else {
            // run 失败 = 服务端没接受,没有产物可删
            return Ok(());
        };

        let mut alive = ctx.alive.lock().unwrap();
        if let Some(pos) = alive.iter().position(|x| x == id) {
            alive.remove(pos);
        }
        Ok(())
    }
}

register_scenario!(UploadScenario);

#[tokio::main]
async fn main() {
    let ctx = Ctx {
        alive: Mutex::new(Vec::new()),
        next_id: AtomicU64::new(1),
    };

    let manager =
        ScenarioManager::new(ManagerConfig::new(4).with_run_mode(RunMode::Times(20)));
    let reports = manager.run_all(&ctx).await;
    println!("{}", reports.report());

    let leftover = ctx.alive.lock().unwrap().len();
    let teardown_failures: u64 = reports.iter().map(|r| r.teardown_failures).sum();
    let failures: u64 = reports.iter().map(|r| r.failures).sum();

    println!("运行结束后残留资源: {leftover}(应为 0 —— 每轮的产物都被 teardown 清掉了)");
    println!("teardown_failures:  {teardown_failures}(单列统计,不污染通过率)");
    println!("failures:           {failures}");
}
