//! 全屏 TUI:顶部实时进度 + 日志滚动区,业务 `tracing` 日志也一并接入。
//!
//! ```text
//! cargo run --example tui --features tui
//! ```

mod shared;

use std::time::Duration;

use memseek_test::{
    LogChannel, Report, RunMode, TaskIndex,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    scenario::Scenario,
    tui::TuiOptions,
};

use shared::DemoError;

#[derive(Default)]
struct Ctx;

#[derive(Default)]
struct TuiScenario;

impl Scenario for TuiScenario {
    type Ctx = Ctx;
    type Error = DemoError;
    type Output = ();
    type Setup = ();

    async fn run(_ctx: &Ctx, task: &TaskIndex, _setup: &()) -> Result<(), DemoError> {
        // 业务日志会出现在 TUI 的日志区(前提是 LogChannel 已挂到 tracing)
        tracing::info!(task = task.index, round = task.round, "处理中");
        tokio::time::sleep(Duration::from_millis(30)).await;
        Ok(())
    }
}

register_scenario!(TuiScenario);

#[tokio::main]
async fn main() {
    let channel = LogChannel::with_capacity(500);

    // 把 LogChannel 当作 writer 挂到 tracing 上,业务日志即进入日志区;
    // 退出 TUI 后缓冲会回放到 stderr,日志不会丢。
    tracing_subscriber::fmt().with_writer(channel.clone()).init();

    // 显示参数;全用默认值时直接 TuiOptions::default().with_channel(channel)
    let mut options = TuiOptions::default();
    options.log_lines = 8;
    options.color = true;

    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Times(60))
            .with_tui_options(options.with_channel(channel)),
    );

    let ctx = Ctx;
    let reports = manager.run_all(&ctx).await;

    // 非 TTY(重定向 / CI)下 TUI 自动静默,所以照样能拿到完整报告
    println!("{}", reports.report());
}
