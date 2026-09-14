//! 实时进度端到端测试:开启进度不改变统计口径,渲染任务的启停不干扰运行。
//!
//! 渲染文本本身由 `src/progress.rs` 的单元测试覆盖(可替换出口断言);
//! 这里覆盖 Manager → RunnerConfig → Recorder 的完整注入链路。

use std::{
    borrow::Cow,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use memseek_test::{
    ProgressOptions, RunMode, Shutdown, TaskIndex,
    error::ScenarioError,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    scenario::Scenario,
};

#[derive(Debug)]
struct TestError;

impl ScenarioError for TestError {
    fn kind(&self) -> Cow<'static, str> {
        Cow::Borrowed("test")
    }
}

#[derive(Default)]
struct ProgressCtx {
    rounds: AtomicU64,
}

#[derive(Default)]
struct ProgressScenario;

impl Scenario for ProgressScenario {
    type Ctx = ProgressCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(ctx: &ProgressCtx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        ctx.rounds.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        Ok(())
    }
}

register_scenario!(ProgressScenario);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// 强制输出(测试环境 stderr 非 TTY),覆盖渲染任务真实启停路径
fn forced() -> ProgressOptions {
    ProgressOptions {
        force: true,
        refresh: Duration::from_millis(5),
        ..ProgressOptions::default()
    }
}

#[test]
fn progress_keeps_times_report_accurate() {
    let ctx = ProgressCtx::default();
    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Times(40))
            .with_progress(),
    );

    let reports = runtime().block_on(manager.run_all(&ctx));

    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!(report.times, 40);
    assert_eq!(report.success, 40);
    assert_eq!(report.failures, 0);
    assert_eq!(ctx.rounds.load(Ordering::SeqCst), 40);
}

#[test]
fn progress_survives_graceful_shutdown() {
    let ctx = ProgressCtx::default();
    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Times(100_000))
            .with_progress_options(forced()),
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
            manager.run_one_with_shutdown::<ProgressCtx>("ProgressScenario", &ctx, shutdown),
            watch,
        );
        let report = report.expect("scenario should be found");

        // 中断只影响轮数,不影响计数口径:已完成轮次全部计入成功
        assert!(report.interrupted);
        assert!(report.times >= 30, "times = {}", report.times);
        assert_eq!(report.success, report.times);
        assert_eq!(report.failures, 0);

        // 渲染任务可重复启停:第二次运行(较小的 Times)照常出报告
        let again = ScenarioManager::new(
            ManagerConfig::new(2)
                .with_run_mode(RunMode::Times(10))
                .with_progress_options(forced()),
        );
        let second = again
            .run_one::<ProgressCtx>("ProgressScenario", &ctx)
            .await
            .expect("scenario should be found");
        assert_eq!(second.times, 10);
        assert_eq!(second.success, 10);
    });
}

#[test]
fn progress_runs_with_duration_mode() {
    let ctx = ProgressCtx::default();
    let manager = ScenarioManager::new(
        ManagerConfig::new(2)
            .with_run_mode(RunMode::Duration(Duration::from_millis(200)))
            .with_progress_options(forced()),
    );

    let reports = runtime().block_on(manager.run_all(&ctx));

    assert_eq!(reports.len(), 1);
    assert!(reports[0].times > 0);
    assert!(!reports[0].interrupted);
    assert_eq!(reports[0].success, reports[0].times);
}
