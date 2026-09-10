//! 优雅关闭(graceful shutdown)端到端测试。
//!
//! 覆盖:Times/Duration 模式中途取消、Task 粒度 setup 完成后再退出、
//! Round 粒度 setup 每轮前检查、多场景串行时中断后不再启动后续场景、
//! 报告中断标记、Config 注入与显式传参两种入口。

use std::{
    borrow::Cow,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use memseek_test::{
    Report, RunMode, Shutdown, TaskIndex,
    error::ScenarioError,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    registry::ScenarioRegistry,
    scenario::{Scenario, SetupMode},
};

#[derive(Debug)]
struct TestError;

impl ScenarioError for TestError {
    fn kind(&self) -> Cow<'static, str> {
        Cow::Borrowed("test")
    }
}

// ---------- 场景与上下文 ----------

/// 每个并发任务计数各自的完成轮数,用于验证轮次边界停止。
#[derive(Default)]
struct CounterCtx {
    rounds: Mutex<Vec<u64>>,
}

#[derive(Default)]
struct CounterScenario;

impl Scenario for CounterScenario {
    type Ctx = CounterCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(ctx: &CounterCtx, task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        {
            let mut rounds = ctx.rounds.lock().unwrap();
            if rounds.len() <= task.index {
                rounds.resize(task.index + 1, 0);
            }
            rounds[task.index] += 1;
        }
        // 模拟真实异步工作(run 内有 IO 时会让出调度,观察者协程才能插入检查点)
        tokio::task::yield_now().await;
        Ok(())
    }
}

register_scenario!(CounterScenario);

/// Task 粒度 setup:挂起一段时间,验证信号到达后等 setup 完成再退出。
#[derive(Default)]
struct SetupCtx {
    setup_started: AtomicBool,
}

#[derive(Default)]
struct SlowSetupScenario;

impl Scenario for SlowSetupScenario {
    type Ctx = SetupCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn setup(ctx: &SetupCtx, _task: &TaskIndex) -> Result<(), Self::Error> {
        ctx.setup_started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(())
    }

    async fn run(_ctx: &SetupCtx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        Ok(())
    }
}

register_scenario!(SlowSetupScenario);

/// Round 粒度 setup:每轮 setup+run,验证退出点(总 setups 可能比 runs 多一组)。
#[derive(Default)]
struct RoundCtx {
    setups: Mutex<u64>,
    runs: Mutex<u64>,
}

#[derive(Default)]
struct RoundScenario;

impl Scenario for RoundScenario {
    type Ctx = RoundCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();
    const SETUP_MODE: SetupMode = SetupMode::Round;

    async fn setup(ctx: &RoundCtx, _task: &TaskIndex) -> Result<(), Self::Error> {
        *ctx.setups.lock().unwrap() += 1;
        Ok(())
    }

    async fn run(ctx: &RoundCtx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        *ctx.runs.lock().unwrap() += 1;
        tokio::task::yield_now().await;
        Ok(())
    }
}

register_scenario!(RoundScenario);

/// 多场景串行:第一个场景计数,第二个场景标记"已启动"。
#[derive(Default)]
struct MultiCtx {
    done: Mutex<u64>,
    second_started: AtomicBool,
}

#[derive(Default)]
struct FirstScenario;

impl Scenario for FirstScenario {
    type Ctx = MultiCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(ctx: &MultiCtx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        *ctx.done.lock().unwrap() += 1;
        tokio::task::yield_now().await;
        Ok(())
    }
}

register_scenario!(FirstScenario);

#[derive(Default)]
struct SecondScenario;

impl Scenario for SecondScenario {
    type Ctx = MultiCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(ctx: &MultiCtx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        ctx.second_started.store(true, Ordering::SeqCst);
        tokio::task::yield_now().await;
        Ok(())
    }
}

register_scenario!(SecondScenario);

// ---------- 测试 ----------

fn total_rounds(ctx: &CounterCtx) -> u64 {
    ctx.rounds.lock().unwrap().iter().sum()
}

#[test]
fn times_mode_stops_at_round_boundary() {
    let ctx = CounterCtx::default();
    let manager =
        ScenarioManager::new(ManagerConfig::new(4).with_run_mode(RunMode::Times(100_000)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (tx, shutdown) = Shutdown::new();
        // 观察者:总轮数达阈值后触发优雅关闭
        let watch = async {
            loop {
                if total_rounds(&ctx) >= 100 {
                    tx.cancel();
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        let (report, _) = tokio::join!(
            manager.run_one_with_shutdown::<CounterCtx>("CounterScenario", &ctx, shutdown),
            watch,
        );
        let report = report.expect("scenario should be found");

        assert!(report.interrupted, "应该标记为优雅关闭中断");
        let rounds = ctx.rounds.lock().unwrap();
        assert_eq!(rounds.len(), 4, "并发任务数应为 4");
        let total: u64 = rounds.iter().sum();

        // 信号到达后每个任务至多再完成 1 轮(检查点在每轮开始前)
        assert!((100..=104).contains(&total), "total = {total}");
        // 各任务轮次差不超过 2(观察任务抢占导致 ±1 抖动)
        let (min, max) = (*rounds.iter().min().unwrap(), *rounds.iter().max().unwrap());
        assert!(max - min <= 2, "rounds = {rounds:?}");
        drop(rounds);

        // 已完成轮次全部计入统计,不计为失败
        assert_eq!(report.times, total);
        assert_eq!(report.success, total);
        assert_eq!(report.failures, 0);
        // 报告渲染带中断标记
        assert!(
            report.report().contains("Interrupted"),
            "报告应包含中断标记:\n{}",
            report.report()
        );
    });
}

#[test]
fn duration_mode_stops_early() {
    let ctx = CounterCtx::default();
    let manager = ScenarioManager::new(
        ManagerConfig::new(2).with_run_mode(RunMode::Duration(Duration::from_secs(10))),
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (tx, shutdown) = Shutdown::new();
        let watch = async {
            loop {
                if total_rounds(&ctx) >= 60 {
                    tx.cancel();
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        let start = std::time::Instant::now();
        let (report, _) = tokio::join!(
            manager.run_one_with_shutdown::<CounterCtx>("CounterScenario", &ctx, shutdown),
            watch,
        );
        let report = report.expect("scenario should be found");
        let elapsed = start.elapsed();

        assert!(report.interrupted);
        // 10s 的 Duration 模式应在取消后立即退出
        assert!(elapsed < Duration::from_secs(5), "elapsed = {elapsed:?}");
        assert!(report.times > 0);
    });
}

#[test]
fn task_setup_stops_after_setup_completes() {
    let ctx = SetupCtx::default();
    let manager =
        ScenarioManager::new(ManagerConfig::new(1).with_run_mode(RunMode::Times(100_000)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (tx, shutdown) = Shutdown::new();
        // setup 挂起期间触发取消
        let watch = async {
            while !ctx.setup_started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.cancel();
        };
        let (report, _) = tokio::join!(
            manager.run_one_with_shutdown::<SetupCtx>("SlowSetupScenario", &ctx, shutdown),
            watch,
        );
        let report = report.expect("scenario should be found");

        // setup 完整执行(300ms 睡完),随后在进入 run 循环前退出
        assert!(ctx.setup_started.load(Ordering::SeqCst));
        assert!(report.interrupted);
        assert_eq!(report.times, 0, "setup 后不应再启动 run 轮次");
        assert_eq!(report.success, 0);
    });
}

#[test]
fn round_setup_stops_between_rounds() {
    let ctx = RoundCtx::default();
    let manager =
        ScenarioManager::new(ManagerConfig::new(3).with_run_mode(RunMode::Times(100_000)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (tx, shutdown) = Shutdown::new();
        let watch = async {
            loop {
                if *ctx.setups.lock().unwrap() >= 30 {
                    tx.cancel();
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        let (report, _) = tokio::join!(
            manager.run_one_with_shutdown::<RoundCtx>("RoundScenario", &ctx, shutdown),
            watch,
        );
        let report = report.expect("scenario should be found");

        let setups = *ctx.setups.lock().unwrap();
        let runs = *ctx.runs.lock().unwrap();

        assert!(report.interrupted);
        // 退出前每个任务至多再完成一组 setup+run
        assert!((30..=33).contains(&setups), "setups = {setups}");
        assert!((27..=33).contains(&runs), "runs = {runs}");
        // Round 粒度:退出点在循环头,总 setups 可能比 runs 多一组
        assert!(
            setups >= runs && setups <= runs + 3,
            "setups = {setups}, runs = {runs}"
        );
        assert_eq!(report.times, runs);
    });
}

#[test]
fn manager_stops_scenarios_after_shutdown() {
    let ctx = MultiCtx::default();
    let (tx, shutdown) = Shutdown::new();
    let manager = ScenarioManager::new(
        ManagerConfig::new(2)
            .with_run_mode(RunMode::Times(100_000))
            .with_shutdown(shutdown),
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let first = ScenarioRegistry::find::<MultiCtx>("FirstScenario").unwrap();
        let second = ScenarioRegistry::find::<MultiCtx>("SecondScenario").unwrap();

        let watch = async {
            loop {
                if *ctx.done.lock().unwrap() >= 20 {
                    tx.cancel();
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        // 通过 ManagerConfig::with_shutdown 注入(Config 注入路径)
        let scenarios = [first, second];
        let (reports, _) = tokio::join!(manager.run::<MultiCtx>(&ctx, &scenarios), watch);

        assert_eq!(reports.len(), 1, "中断后不应再启动后续场景");
        assert!(reports[0].interrupted);
        assert!(
            !ctx.second_started.load(Ordering::SeqCst),
            "第二个场景不应被执行"
        );
        // 多场景汇总渲染带中断标记
        assert!(
            reports.report().contains("Interrupted"),
            "汇总报告应包含中断标记:\n{}",
            reports.report()
        );
    });
}
