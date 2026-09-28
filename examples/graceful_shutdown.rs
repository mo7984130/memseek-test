//! 优雅关闭:用 `Shutdown` 程序化收尾,以及按注册名只跑一个场景。
//!
//! ```text
//! cargo run --example graceful_shutdown
//! ```

mod shared;

use std::time::Duration;

use memseek_test::{
    Report, RunMode, Shutdown, TaskIndex,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    scenario::Scenario,
};

use shared::DemoError;

#[derive(Default)]
struct Ctx;

/// 每轮花 50ms,好让「收尾」有东西可收。
#[derive(Default)]
struct SlowScenario;

impl Scenario for SlowScenario {
    type Ctx = Ctx;
    type Error = DemoError;
    type Output = ();
    type Setup = ();

    async fn run(_ctx: &Ctx, _task: &TaskIndex, _setup: &()) -> Result<(), DemoError> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(())
    }
}

register_scenario!(SlowScenario);

#[tokio::main]
async fn main() {
    let ctx = Ctx;

    // ─────────────────────────────────────────────────────────────
    // 用法一:时长设成 1 小时,靠外部信号在 2 秒后收尾
    // 触发后:正在跑的请求正常完成并计入统计,不再发起新轮次,
    // 报告里 interrupted 会被置为 true。
    // ─────────────────────────────────────────────────────────────
    let (tx, shutdown) = Shutdown::new();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        tx.cancel();
    });

    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Duration(Duration::from_secs(3600)))
            .with_shutdown(shutdown),
    );
    let reports = manager.run_all(&ctx).await;
    println!("{}", reports.report());
    println!(
        "报告标注 interrupted = {}",
        reports.iter().any(|r| r.interrupted)
    );

    // ─────────────────────────────────────────────────────────────
    // 用法二:按注册名只跑一个场景
    // 名字写错时返回 None,不会静默变成「跑了 0 个场景」。
    //
    // 注意:`SlowScenario` 注册时没有指定 mode(见文件末尾的
    // `register_scenario!(SlowScenario);`),所以必须由 Manager 补上模式,
    // 否则会 panic「未配置执行模式(times/duration)」——两种来源都没有时
    // 框架无法知道该跑多少轮。
    // ─────────────────────────────────────────────────────────────
    let (tx, shutdown) = Shutdown::new();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        tx.cancel();
    });

    let manager = ScenarioManager::new(
        ManagerConfig::new(2).with_run_mode(RunMode::Duration(Duration::from_secs(3600))),
    );
    match manager
        .run_one_with_shutdown::<Ctx>("SlowScenario", &ctx, shutdown)
        .await
    {
        Some(report) => {
            println!("{}", report.report());
            println!("单场景 interrupted = {}", report.interrupted);
        }
        None => eprintln!("场景未注册: SlowScenario"),
    }

    // ─────────────────────────────────────────────────────────────
    // 用法三:Ctrl-C 优雅停止(终端里压测的典型形态)
    // install_ctrl_c() 在后台监听 SIGINT。下面这段会一直跑到你按 Ctrl-C,
    // 所以默认注释掉,想试的时候取消注释:
    //
    // let manager = ScenarioManager::new(
    //     ManagerConfig::new(8)
    //         .with_run_mode(RunMode::Duration(Duration::from_secs(3600)))
    //         .install_ctrl_c(),
    // );
    // let reports = manager.run_all(&ctx).await;
    // println!("{}", reports.report());
    // ─────────────────────────────────────────────────────────────
}
