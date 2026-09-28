# memseek-test

[![crates.io](https://img.shields.io/crates/v/memseek-test.svg)](https://crates.io/crates/memseek-test)
[![docs.rs](https://img.shields.io/docsrs/memseek-test)](https://docs.rs/memseek-test)
[![license](https://img.shields.io/crates/l/memseek-test.svg)](#license)

> 基于场景的异步**正确性验证**与压测框架 —— 同一套 `Scenario`,既是接口正确性的断言用例,也是压测负载。

*Scenario-based async correctness verification and load testing for Rust. One `Scenario` gives you both an assertion suite and a load generator.*

## 为什么不是"又一个压测框架"

多数压测工具只回答"**快不快**"(RPS、p99、吞吐),而接口"**对不对**"通常靠另一套 e2e 测试回答。两边的用例几乎一样——同一批接口、同一份请求体、同一套准备数据——却要写两遍。

`memseek-test` 把两件事合并到同一个场景里:

- `RunMode::Times(n)` —— 跑固定轮次,把每个 `Scenario` 当**正确性断言**执行,失败率/超时率超标即 CI 退出码非 0;
- `RunMode::Duration(d)` —— 跑固定时长,同一份场景直接当**压测**用,输出 HDR 直方图延迟分位。

`Scenario` 的 `run` 负责发请求,`validate` 负责断言 —— 断言不只看状态码,还可以拿响应和数据库 / Redis / 对象存储**对账**(这正是它在真实后端 e2e 里的用法)。

## 核心能力

- **正确性断言是一等公民**: `validate` 阶段独立统计(`validate_success` / `validate_failures`),断言失败与请求失败分开计数。
- **四段生命周期**: `setup` → `run` → `validate` → `teardown`,后两段可省略;`setup` / `teardown` 支持按任务或按轮执行。
- **teardown 是 finally 语义**: 无论 `run` 成功失败都会执行,且失败被单列(`teardown_failures`),不会污染通过率。
- **对账友好**: 场景上下文是任意 `Send + Sync` 类型,可挂载数据库连接池、Redis、S3 客户端,断言时直接查库比对。
- **HDR 直方图报告**: `min` / `avg` / `p50` / `p95` / `p99` / RPS,按场景输出,内建 ANSI 柱状图渲染。
- **优雅关闭**: Ctrl-C 或程序化触发,正在跑的请求正常收尾并计入统计,报告标注 `interrupted`。
- **超时退避**: 超时视为过载信号,指数退避降低施压速率,过载时可关闭。
- **多线程分片**: `with_workers(n)` 把并发任务切成 n 个分片跑在独立 OS 线程上。
- **可选全屏 TUI**: 实时进度条 + 日志滚动区,业务 `tracing` 日志可直接接入日志区。
- **HTTP 辅助**: 基于 `reqwest` 的客户端封装,带默认超时、响应体快照、错误分类。

## 安装

```toml
[dependencies]
memseek-test = "0.24"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }

# 需要全屏 TUI 时:
# memseek-test = { version = "0.24", features = ["tui"] }
```

需要 Rust 1.88+(edition 2024;此下界由 `hdrhistogram` 与 url→idna→icu 依赖链决定),并且请使用**多线程** tokio 运行时——TUI 渲染与 Ctrl-C 监听都是在后台 spawn 出来的,`current_thread` 运行时下它们会被饿死(框架检测到会给出警告)。

## 五分钟上手

```rust
use memseek_test::{
    Report, RunMode, TaskIndex, register_scenario,
    scenario::Scenario,
    ctxlibs::http_client::{Client, HttpError, reqwest},
    manager::{ManagerConfig, ScenarioManager},
};

/// 全局上下文: 被测地址 + 复用的 HTTP 客户端(连接池共享)
struct Ctx {
    client: Client,
}

/// 健康检查: 期待 200, 且响应体含 "ok"
#[derive(Default)]
struct HealthScenario;

impl Scenario for HealthScenario {
    type Ctx = Ctx;
    type Error = HttpError;
    /// run 的产物, 原样交给 validate 断言
    type Output = String;
    /// setup 的产物; 无前置准备时用 ()
    type Setup = ();

    async fn run(ctx: &Ctx, _task: &TaskIndex, _setup: &()) -> Result<String, HttpError> {
        let body = ctx
            .client
            .request(reqwest::Method::GET, "/health")
            .send_checked() // 非 2xx 直接变成 HttpError::Status, 不会静默放过
            .await?
            .text()
            .await?;
        Ok(body)
    }

    async fn validate(
        _ctx: &Ctx,
        _task: &TaskIndex,
        _setup: &(),
        body: &String,
    ) -> Result<bool, HttpError> {
        Ok(body.contains("ok"))
    }
}

register_scenario!(HealthScenario);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = Ctx { client: Client::new("http://localhost:8080")? };

    let manager = ScenarioManager::new(
        ManagerConfig::new(8)                   // 并发 8
            .with_run_mode(RunMode::Times(200)) // 跑 200 轮做正确性验收
            .install_ctrl_c(),                  // Ctrl-C 优雅停止
    );

    let reports = manager.run_all(&ctx).await;
    println!("{}", reports.report());

    // 汇总判定 -> CI 退出码
    let bad: u64 = reports.iter().map(|r| r.failures + r.validate_failures).sum();
    if bad > 0 {
        std::process::exit(1);
    }
    Ok(())
}
```

`register_scenario!` 在编译期链接期登记场景(基于 `inventory`),`run_all` 只会运行 `Ctx` 类型匹配的场景。想改名或指定默认模式:

```rust
register_scenario!(HealthScenario, name = "health", mode = RunMode::Times(1_000));
```

## 可运行示例

`examples/` 下每个示例对应上面一个主题,都能直接跑:

| 示例 | 跑法 | 演示什么 |
| --- | --- | --- |
| `quickstart` | `cargo run --example quickstart` | `Times` 模式 + `validate` 对账 + CI 判定,**不需要任何外部服务** |
| `http` | `cargo run --example http` | `Client`、请求构造、`send_checked`、错误分类(需先起一个本地 HTTP 服务) |
| `graceful_shutdown` | `cargo run --example graceful_shutdown` | 程序化优雅停止、按名执行单个场景、`report.interrupted` |
| `per_round_setup` | `cargo run --example per_round_setup` | 按轮的 `setup`/`teardown`、finally 语义、`teardown_failures` 单列 |
| `tui` | `cargo run --example tui --features tui` | 全屏进度与日志滚动区接线 |

## 真实用法: 后端 e2e 正确性套件

下面摘自一个真实 Rust 后端项目的 e2e 套件(63 个场景:auth 8 / user 22 / visual 32 / system 1)——它的主用途就是**上线前的正确性验收**:`run` 打接口,`validate` 拿响应和数据库逐字段对账。

### 1. 上下文里挂上"对账用的眼"

```rust
pub struct Ctx {
    /// 被测服务的 HTTP 客户端
    pub client: Client,
    /// 直连被测服务同一个库, 用于断言响应与库中记录一致
    pub db: Db,
    /// 其它旁证: 邮件服务、Redis、对象存储……
    pub mailhog: Client,
    pub redis: RedisPool,
    pub s3: S3Client,
}
```

### 2. `setup` 里登录拿会话(每个并发任务一次)

```rust
/// 会话预置: 每个并发任务用自己池子里的账号登录, 凭证随任务走
pub struct Session {
    pub user_id: i64,
    pub access_token: String,
}

impl Default for Session { /* ... */ }

impl Session {
    pub fn auth_header(&self) -> String {
        format!("Bearer {} {}", self.user_id, self.access_token)
    }
}

#[derive(Default)]
pub struct MeScenario;

impl Scenario for MeScenario {
    type Ctx = Ctx;
    type Error = HttpError;
    type Output = UserInfo;
    type Setup = Session;

    // 默认 SETUP_MODE = Task: 每个并发任务登录一次, 后续每轮复用
    async fn setup(ctx: &Ctx, task: &TaskIndex) -> Result<Session, HttpError> {
        let resp = ctx
            .client
            .request(reqwest::Method::POST, "/auth/login")
            .json_unwrap(&json!({
                "account":  format!("uit_user_{}", task.index + 1),
                "password": "Test123456",
            }))
            .send_checked()
            .await?
            .json::<LoginResponse>()
            .await?;
        Ok(Session { user_id: resp.user.id, access_token: resp.access_token })
    }

    async fn run(ctx: &Ctx, _task: &TaskIndex, setup: &Session) -> Result<UserInfo, HttpError> {
        ctx.client
            .request(reqwest::Method::GET, "/user/me")
            .header("Authorization", &setup.auth_header())
            .send_checked()
            .await?
            .json::<UserInfo>()
            .await
            .map_err(HttpError::from)
    }

    /// 正确性断言: 响应必须与库中记录一致
    async fn validate(
        ctx: &Ctx,
        _task: &TaskIndex,
        setup: &Session,
        out: &UserInfo,
    ) -> Result<bool, HttpError> {
        let user = ctx.db.find_user(setup.user_id).await?;
        Ok(out.id == setup.user_id
            && out.username == user.username
            && out.email == user.email
            && out.nickname == user.nickname)
    }
}

register_scenario!(MeScenario);
```

负例场景同样只是断言,不需要额外机制:

```rust
/// 未携带认证头: 期望 401
async fn validate(_: &Ctx, _: &TaskIndex, _: &(), out: &ErrR) -> Result<bool, HttpError> {
    Ok(out.code == 401)
}
```

### 3. 按轮的 `setup` / `teardown` 做资源清理

写场景每轮都要新的输入、并要清掉本轮产物:

```rust
use serde_json::json;

impl Scenario for UploadVisualScenario {
    type Ctx = Ctx;
    type Error = HttpError;
    type Output = VisualId;
    type Setup = UploadSetup;

    const SETUP_MODE: SetupMode = SetupMode::Round;          // 每轮准备一份待上传数据
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Round; // 每轮结束清掉本轮产物

    async fn run(ctx: &Ctx, _task: &TaskIndex, _setup: &UploadSetup) -> Result<VisualId, HttpError> {
        /* ... 上传, 返回本轮创建的 id ... */
    }

    async fn teardown(
        ctx: &Ctx,
        _task: &TaskIndex,
        setup: &UploadSetup,
        result: Option<Result<&VisualId, &HttpError>>, // finally 语义: run 成功失败都会进来
    ) -> Result<(), HttpError> {
        let Some(Ok(id)) = result else {
            return Ok(()); // run 失败 = 服务端没接受, 无产物可删
        };

        let response = ctx
            .client
            .request(reqwest::Method::DELETE, "/visual")
            .header("Authorization", &setup.session.auth_header())
            .json_unwrap(&json!({ "visualIds": [id.0] }))
            .send()
            .await?;

        // 幂等清理: 404 视为"本来就不存在", 不算收尾失败
        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(HttpError::status(status, reqwest::Method::DELETE, &ctx.client.base_url))
    }
}

register_scenario!(UploadVisualScenario);
```

### 4. 按白名单/黑名单挑场景,汇总判定给 CI

```rust
let all = ScenarioRegistry::scenarios::<Ctx>();
let selected: Vec<_> = all
    .into_iter()
    .filter(|s| whitelist.is_empty() || whitelist.contains(&s.name))
    .filter(|s| !blacklist.contains(&s.name))
    .collect();

let reports = manager.run(&ctx, &selected).await;
println!("{}", reports.report_with(ReportOptions {
    color: std::io::stdout().is_terminal(),
    ..Default::default()
}));

let times: u64 = reports.iter().map(|r| r.times).sum();
let failures: u64 = reports.iter().map(|r| r.failures + r.validate_failures).sum();
let timeouts: u64 = reports.iter().map(|r| r.timeouts).sum();
let interrupted = reports.iter().any(|r| r.interrupted);

if times == 0 || failures > 0 || timeouts > 0 || interrupted {
    eprintln!("判定: FAIL");
    std::process::exit(1);
}
```

> 名单里写错场景名要直接退出报错,别让它退化成"静默少跑"。

## 生命周期语义

```
setup ──► run ──► validate ──► teardown
```

| 阶段 | 何时跑 | 失败如何记账 |
| --- | --- | --- |
| `setup` | 按任务(默认)或按轮 | 判一轮失败;若为**超时**则触发退避重试 |
| `run` | 必填,每轮一次 | 记 `failures` 或 `timeouts`,**并跳过 validate** |
| `validate` | 仅当 `run` 成功 | `Ok(false)` 记为 `validate` 分类;`Err` 按 `Error::kind()` 分类;两者都计入 `validate_failures` |
| `teardown` | **finally**,退出前必跑 | 只影响 `teardown_failures` + `teardown:{kind}`,不影响通过率、不触发退避 |

`setup` / `teardown` 的耗时**不计入**延迟分位(它们不属于 `run`),但会拖慢轮次节奏,因此报告的 RPS 反映的是含准备与收尾的整体吞吐。

### 错误分类

实现 `ScenarioError` 即可自定义分类;框架内建的 `HttpError` 已按下列类别归类,直接体现在报告的 `error_map` 里:

```
http_status_404   timeout   connect_refused   connect_dns   connect_unreachable   serialize   decode   ...
```

对未包装的原生 `reqwest` 请求,框架同样为 `reqwest::Error` 实现了 `ScenarioError`,归类方式一致。

## 运行模式与执行控制

```rust
ManagerConfig::new(64)                         // 并发度(0 会 panic)
    .with_run_mode(RunMode::Duration(Duration::from_secs(120)))
    .with_workers(4)                           // 切成 4 个分片跑在独立 OS 线程
    .with_backoff(BackoffConfig::new(       // 超时退避: 100ms 起, 上限 3s, 倍率 2
        Duration::from_millis(100),
        Duration::from_secs(3),
        2.0,
    ))
    .with_shutdown(shutdown)                   // 程序化触发优雅停止
    .install_ctrl_c()                          // 或直接监听 SIGINT
```

- **`RunMode::Times(n)`** —— 正确性验收。`n` 是**所有并发任务之和**的总轮次。
- **`RunMode::Duration(d)`** —— 压测。到点停止,正在跑的请求收尾后计入统计。
- **`with_workers(n)`** —— 分片并行。`task.index` / `task.total` 仍全局一致,报告口径不受影响;`n` 会被夹到并发度以内。
- **退避** —— 只在**超时**时触发;压测场景建议显式关闭,否则过载时框架会自动减速,压不出目标速率。

程序化停止:

```rust
let (tx, shutdown) = Shutdown::new();
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(30)).await;
    tx.cancel();
});
```

## 报告

```rust
println!("{}", report.report());                             // 纯文本块
println!("{}", reports.report_with(ReportOptions {           // 控制颜色与柱宽
    color: true,
    bar_width: 40,
}));
println!("{}", reports.report_with_color());                  // 快捷方式
```

`Report` 已为单条 `ScenarioReport`、`&[ScenarioReport]` 和 `Vec<ScenarioReport>` 实现,所以 `reports.report()` 直接可用。

`ScenarioReport` 里对判定最有用的字段:`times`、`success`、`failures`、`validate_success`、`validate_failures`、`timeouts`、`teardown_failures`、`error_map`、`interrupted`,以及 `p50` / `p95` / `p99` / `avg` / `min` / `max` 与各阶段耗时。

## HTTP 辅助

```rust
use memseek_test::ctxlibs::http_client::{Client, CaptureOptions};

// 默认 3s 超时; 需要完整控制(连接池/TLS/代理)时用 from_reqwest
let client = Client::new("http://localhost:8080")?;
// 或: Client::from_reqwest(reqwest::Client::builder().timeout(...).build()?, base)?;

let resp = client
    .request(reqwest::Method::POST, "/visual")
    .header("Authorization", &token)
    .query("page", "1")
    .json(&payload)?          // 序列化失败返回 Err
    .send_checked()           // 非 2xx -> HttpError::Status, 并保留请求/响应体快照
    .await?;

// 高 QPS 下关掉响应体快照, 省内存
let client = client.with_capture(CaptureOptions::disabled());
```

- `send()` 不做状态码校验;`send_checked()` 把非 2xx 变成错误,**并带上请求/响应体快照**,失败时能在报告里看到现场。
- 相对路径会保留 `base_url` 的路径前缀(`.../api` + `/auth` → `.../api/auth`)。
- `Client` 实现了 `Deref<Target = reqwest::Client>`,原生 reqwest API 随时可用;`reqwest` 与 `reqwest::multipart` 已从本 crate 重导出,保证版本一致。

## TUI(feature = `tui`)

```rust
let channel = LogChannel::with_capacity(500);

tracing_subscriber::fmt()
    .with_writer(channel.clone())   // 业务日志直接进 TUI 日志区
    .init();

// 调整显示参数; 全部用默认值时直接 TuiOptions::default().with_channel(channel)
let mut options = TuiOptions::default();
options.log_lines = 12;                                  // 日志区行数
options.refresh = Duration::from_millis(100);            // 刷新间隔
options.bar_width = 40;                                  // 进度条宽度
options.color = true;

let manager = ScenarioManager::new(
    ManagerConfig::new(8).with_tui_options(options.with_channel(channel)),
);
```

全屏显示顶部实时进度 + 日志滚动区;退出后日志缓冲回放到 stderr。非 TTY(重定向 / CI)会自动静默。终端行数不足时进度行会被顶出可视区,调小 `log_lines` 即可。

## 功能开关

| feature | 默认 | 说明 |
| --- | --- | --- |
| `tui` | 否 | 全屏进度 TUI、`LogChannel`、`TuiOptions` |

## 写在最后

`validate` 里的断言就是普通的 Rust 代码 —— 能查数据库、能读 Redis、能比对对象存储,不必迁就任何 DSL。压测与正确性验证共用一个场景,是这个小框架唯一想坚持的事。

## License

MIT
