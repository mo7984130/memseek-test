//! 最小可运行示例:把 `run` 的产物交给 `validate` 断言(`Times` 模式 = 正确性验收)。
//!
//! 不依赖任何外部服务,直接跑:
//!
//! ```text
//! cargo run --example quickstart
//! ```

mod shared;

use std::{collections::HashMap, sync::Mutex};

use memseek_test::{
    Report, RunMode, TaskIndex,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    scenario::Scenario,
};

use shared::DemoError;

/// 全局上下文:真实项目里这里是 HTTP 客户端、数据库连接池、Redis 等共享资源。
/// 示例用一张内存表代替存储层,好让这个例子不依赖外部服务。
struct Ctx {
    balances: Mutex<HashMap<u64, u64>>,
}

/// 转账:扣减余额,并把「扣减后的余额」作为产物交给 `validate`。
#[derive(Default)]
struct TransferScenario;

impl Scenario for TransferScenario {
    type Ctx = Ctx;
    type Error = DemoError;
    /// `run` 的产物,原样交给 `validate` 断言
    type Output = u64;
    /// 无前置准备时用 `()`
    type Setup = ();

    async fn run(ctx: &Ctx, task: &TaskIndex, _setup: &()) -> Result<u64, DemoError> {
        let account = task.index as u64 % 8;
        let balance = {
            let mut balances = ctx.balances.lock().unwrap();
            let balance = balances.entry(account).or_insert(1_000);
            *balance -= 10;
            *balance
        };
        Ok(balance)
    }

    /// 正确性断言:产物必须与存储层里对应账户的余额一致。
    /// 真实验证里这一步通常是一条 `SELECT ...`,然后逐字段比对。
    async fn validate(
        ctx: &Ctx,
        task: &TaskIndex,
        _setup: &(),
        out: &u64,
    ) -> Result<bool, DemoError> {
        let account = task.index as u64 % 8;
        let balances = ctx.balances.lock().unwrap();
        Ok(balances.get(&account) == Some(out))
    }
}

register_scenario!(TransferScenario);

#[tokio::main]
async fn main() {
    let ctx = Ctx { balances: Mutex::new(HashMap::new()) };

    let manager =
        ScenarioManager::new(ManagerConfig::new(4).with_run_mode(RunMode::Times(200)));
    let reports = manager.run_all(&ctx).await;
    println!("{}", reports.report());

    // ── 汇总判定,直接当作 CI 退出码 ──
    // `failures` 是 run 失败,`validate_failures` 是断言未通过 —— 两者分开统计。
    let bad: u64 = reports
        .iter()
        .map(|r| r.failures + r.validate_failures)
        .sum();
    if bad > 0 {
        eprintln!("判定: FAIL({bad} 次未通过)");
        std::process::exit(1);
    }

    let account = 0;
    println!(
        "判定: PASS(counter#{account} 余额 = {})",
        ctx.balances.lock().unwrap().get(&account).copied().unwrap_or(0)
    );
}
