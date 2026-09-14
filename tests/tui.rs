//! TUI 模式端到端测试(feature `tui`):开启全屏进度/日志不影响统计,
//! 日志通道接线正常,优雅关闭下照常收尾。
//!
//! 测试环境 stderr 非 TTY:TUI 自动静默(不进 alternate screen),
//! 仅验证 Manager 集成链路与报告口径。

#![cfg(feature = "tui")]

use std::{
    borrow::Cow,
    sync::atomic::{AtomicU64, Ordering},
};

use memseek_test::{
    LogChannel, RunMode, Shutdown, TaskIndex,
    error::ScenarioError,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    scenario::Scenario,
    tui::TuiOptions,
};

#[derive(Debug)]
struct TestError;

impl ScenarioError for TestError {
    fn kind(&self) -> Cow<'static, str> {
        Cow::Borrowed("test")
    }
}

#[derive(Default)]
struct TuiCtx {
    rounds: AtomicU64,
}

#[derive(Default)]
struct TuiScenario;

impl Scenario for TuiScenario {
    type Ctx = TuiCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(ctx: &TuiCtx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        ctx.rounds.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        Ok(())
    }
}

register_scenario!(TuiScenario);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// 用户显式传入日志通道(模拟 with_channel 接入业务日志的用法)
#[test]
fn tui_mode_keeps_report_accurate() {
    let ctx = TuiCtx::default();
    let channel = LogChannel::with_capacity(50);
    let options = TuiOptions::default().with_channel(channel.clone());
    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Times(20))
            .with_tui_options(options),
    );

    let reports = runtime().block_on(manager.run_all(&ctx));

    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!(report.times, 20);
    assert_eq!(report.success, 20);
    assert_eq!(report.failures, 0);
    assert_eq!(ctx.rounds.load(Ordering::SeqCst), 20);
    // 运行结束:日志缓冲已被 Manager 回放到 stderr 并清空
    assert!(channel.lines().is_empty());
}

/// 默认选项(内建通道)不 panic,报告正常
#[test]
fn tui_default_options_run_passes() {
    let ctx = TuiCtx::default();
    let manager = ScenarioManager::new(
        ManagerConfig::new(2)
            .with_run_mode(RunMode::Times(6))
            .with_tui(),
    );

    let reports = runtime().block_on(manager.run_all(&ctx));

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].times, 6);
    assert_eq!(reports[0].success, 6);
}

/// 优雅关闭:TUI 渲染任务随场景收尾,统计口径不变,信号后再跑一次也正常
#[test]
fn tui_mode_survives_graceful_shutdown() {
    let ctx = TuiCtx::default();
    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Times(100_000))
            .with_tui(),
    );

    runtime().block_on(async {
        let (tx, shutdown) = Shutdown::new();
        let watch = async {
            while ctx.rounds.load(Ordering::SeqCst) < 30 {
                tokio::task::yield_now().await;
            }
            tx.cancel();
        };
        let (report, _) = tokio::join!(
            manager.run_one_with_shutdown::<TuiCtx>("TuiScenario", &ctx, shutdown),
            watch,
        );
        let report = report.expect("scenario should be found");

        assert!(report.interrupted);
        assert!(report.times >= 30, "times = {}", report.times);
        assert_eq!(report.success, report.times);
        assert_eq!(report.failures, 0);
    });
}
