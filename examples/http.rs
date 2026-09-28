//! HTTP 辅助:请求构造、状态码校验、错误分类。
//!
//! 前置:先在本地起一个会返回 200 的 HTTP 服务,例如
//!
//! ```text
//! python3 -m http.server 7985
//! cargo run --example http
//! ```

use std::time::Duration;

use memseek_test::{
    Report, RunMode, TaskIndex,
    ctxlibs::http_client::{Client, HttpError, reqwest},
    error::ScenarioError,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    scenario::Scenario,
};

/// 复用一个 `Client`:连接池在整个运行期间共享。
struct Ctx {
    client: Client,
}

/// GET /:期待 200。`send_checked` 会把非 2xx 直接变成 `HttpError::Status`。
#[derive(Default)]
struct HealthScenario;

impl Scenario for HealthScenario {
    type Ctx = Ctx;
    type Error = HttpError;
    type Output = ();
    type Setup = ();

    async fn run(ctx: &Ctx, _task: &TaskIndex, _setup: &()) -> Result<(), HttpError> {
        ctx.client
            .request(reqwest::Method::GET, "/")
            .send_checked()
            .await?;
        Ok(())
    }
}

register_scenario!(HealthScenario);

/// GET /missing:期待 404 —— 用 `validate` 断言「失败得符合预期」,
/// 这样负例场景也是正常的一等公民,而不是被跳过。
#[derive(Default)]
struct NotFoundScenario;

impl Scenario for NotFoundScenario {
    type Ctx = Ctx;
    type Error = HttpError;
    /// 把错误本身当作产物交给 `validate`
    type Output = Option<HttpError>;
    type Setup = ();

    async fn run(
        ctx: &Ctx,
        _task: &TaskIndex,
        _setup: &(),
    ) -> Result<Option<HttpError>, HttpError> {
        let outcome = ctx
            .client
            .request(reqwest::Method::GET, "/missing")
            .send_checked()
            .await
            .err();
        Ok(outcome)
    }

    async fn validate(
        _ctx: &Ctx,
        _task: &TaskIndex,
        _setup: &(),
        out: &Option<HttpError>,
    ) -> Result<bool, HttpError> {
        // kind() 的分类直接来自框架的错误归类,不需要自己解析状态码
        Ok(out.as_ref().is_some_and(|e| e.kind() == "http_status_404"))
    }
}

register_scenario!(NotFoundScenario);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://localhost:7985".to_string());

    // 需要连接池 / TLS / 代理等完整控制时用 from_reqwest;
    // Client::new(base) 则会套一个 3 秒的默认超时。
    let client = Client::from_reqwest(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()?,
        base.as_str(),
    )?;

    let ctx = Ctx { client };
    let manager =
        ScenarioManager::new(ManagerConfig::new(8).with_run_mode(RunMode::Times(50)));
    let reports = manager.run_all(&ctx).await;
    println!("{}", reports.report());

    // 错误明细按分类计数,直接告诉你失败集中在哪一类
    let errors: u64 = reports
        .iter()
        .map(|r| r.error_map.values().sum::<u64>())
        .sum();
    if errors == 0 {
        println!("本次运行没有错误。(若服务没起,这里会列出 connect_refused 等分类计数)");
    } else {
        for report in &reports {
            for (kind, count) in &report.error_map {
                println!("  {} -> {kind} x{count}", report.name);
            }
        }
    }

    Ok(())
}
