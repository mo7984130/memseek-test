//! `teardown` 收尾阶段:finally 语义、粒度、计数口径与边界。
//!
//! 关键约定(与文档同步):
//! - `run`/`validate` 无论成败都会收尾;`Round` 粒度携带本轮结果,`Task` 粒度恒为 `None`;
//! - 收尾失败只记 `teardown_failures`(+`teardown:{kind}` 明细),不加剧 `failures`/`timeouts`,
//!   不触发退避,也不中止任务;
//! - `setup` 未成功时不执行任务级收尾。

use std::{
    borrow::Cow,
    sync::{Mutex, atomic::AtomicU64},
    time::Duration,
};

use memseek_test::{
    RunMode, Shutdown, ShutdownSender,
    error::ScenarioError,
    runner::{RunnerConfig, ScenarioRunner, TaskIndex},
    scenario::{Scenario, SetupMode, TeardownMode},
};

#[derive(Debug)]
enum TestErr {
    Boom,
    Cleanup,
}

impl ScenarioError for TestErr {
    fn kind(&self) -> Cow<'static, str> {
        Cow::Borrowed(match self {
            Self::Boom => "boom",
            Self::Cleanup => "cleanup",
        })
    }
}

/// 事件日志:按发生顺序记录各阶段调用(供顺序/计数断言)。
#[derive(Default)]
struct Events {
    log: Mutex<Vec<String>>,
    shutdown: Mutex<Option<ShutdownSender>>,
}

impl Events {
    fn push(&self, event: impl Into<String>) {
        self.log.lock().unwrap().push(event.into());
    }

    fn drain(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn arm_shutdown(&self, sender: ShutdownSender) {
        *self.shutdown.lock().unwrap() = Some(sender);
    }

    fn fire_shutdown(&self) {
        if let Some(sender) = self.shutdown.lock().unwrap().as_ref() {
            sender.cancel();
        }
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn runner_config(mode: RunMode, shutdown: Option<Shutdown>, task_total: usize) -> RunnerConfig {
    RunnerConfig {
        mode,
        task_index: 0,
        task_total,
        shutdown,
        backoff: None,
        #[cfg(feature = "tui")]
        progress: None,
    }
}

// ---------- 轮级收尾:顺序、finally、耗时记账 ----------

/// 轮级收尾:每轮 run → validate → teardown,且各阶段耗时单独记账。
#[derive(Default)]
struct RoundTeardown;

impl Scenario for RoundTeardown {
    type Ctx = Events;
    type Error = TestErr;
    type Output = usize;
    type Setup = ();
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Round;

    async fn setup(ctx: &Self::Ctx, task: &TaskIndex) -> Result<(), Self::Error> {
        ctx.push(format!("setup:{}", task.round));
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(())
    }

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &()) -> Result<usize, Self::Error> {
        ctx.push(format!("run:{}", task.round));
        Ok(task.round)
    }

    async fn validate(
        ctx: &Self::Ctx,
        task: &TaskIndex,
        _setup: &(),
        output: &usize,
    ) -> Result<bool, Self::Error> {
        ctx.push(format!("validate:{}", task.round));
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(*output == task.round)
    }

    async fn teardown(
        ctx: &Self::Ctx,
        task: &TaskIndex,
        _setup: &(),
        result: Option<Result<&usize, &TestErr>>,
    ) -> Result<(), Self::Error> {
        let round = match result.expect("轮级收尾应携带本轮结果") {
            Ok(output) => format!("ok:{output}"),
            Err(err) => format!("err:{err:?}"),
        };
        ctx.push(format!("teardown:{}:{round}", task.round));
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(())
    }
}

#[test]
fn round_teardown_runs_after_validate_and_is_measured() {
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner =
            ScenarioRunner::<RoundTeardown>::new(runner_config(RunMode::Times(3), None, 1));
        runner.run(&ctx).await
    });

    assert_eq!(
        ctx.drain(),
        vec![
            "setup:0",
            "run:0",
            "validate:0",
            "teardown:0:ok:0",
            "run:1",
            "validate:1",
            "teardown:1:ok:1",
            "run:2",
            "validate:2",
            "teardown:2:ok:2",
        ],
        "任务级 setup 只执行一次,轮内顺序为 run → validate → teardown"
    );
    assert_eq!(recorder.teardown_success, 3);
    assert_eq!(recorder.teardown_failures, 0);
    assert_eq!(recorder.success, 3);
    assert_eq!(recorder.validate_success, 3);
    // 各阶段耗时单独记账(退避/报告据此解释 RPS)
    assert!(recorder.setup_total >= Duration::from_millis(1));
    assert!(recorder.validate_total >= Duration::from_millis(3));
    assert!(recorder.teardown_total >= Duration::from_millis(3));
}

/// `run` 失败时依然收尾,且拿到 `Err`;validate 被跳过。
#[derive(Default)]
struct FailingRound;

impl Scenario for FailingRound {
    type Ctx = Events;
    type Error = TestErr;
    type Output = ();
    type Setup = ();
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Round;

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        if task.round == 0 {
            ctx.push("run:0:err");
            Err(TestErr::Boom)
        } else {
            ctx.push("run:1:ok");
            Ok(())
        }
    }

    async fn validate(
        ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &(),
        _output: &(),
    ) -> Result<bool, Self::Error> {
        ctx.push("validate");
        Ok(true)
    }

    async fn teardown(
        ctx: &Self::Ctx,
        task: &TaskIndex,
        _setup: &(),
        result: Option<Result<&(), &TestErr>>,
    ) -> Result<(), Self::Error> {
        match result.expect("轮级收尾应携带本轮结果") {
            Ok(()) => ctx.push(format!("teardown:{}:ok", task.round)),
            Err(TestErr::Boom) => ctx.push(format!("teardown:{}:boom", task.round)),
            Err(other) => ctx.push(format!("teardown:{}:{other:?}", task.round)),
        }
        Ok(())
    }
}

#[test]
fn teardown_still_runs_when_run_fails() {
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<FailingRound>::new(runner_config(RunMode::Times(2), None, 1));
        runner.run(&ctx).await
    });

    assert_eq!(
        ctx.drain(),
        vec![
            "run:0:err",
            "teardown:0:boom",
            "run:1:ok",
            "validate",
            "teardown:1:ok"
        ],
        "run 失败跳过 validate,但仍收尾"
    );
    assert_eq!(recorder.failures, 1);
    assert_eq!(recorder.success, 1);
    assert_eq!(recorder.validate_success, 1);
    assert_eq!(recorder.teardown_success, 2);
}

/// 断言失败(validate 返回 false)时依然收尾,拿到 `Ok`。
#[derive(Default)]
struct BadAssert;

impl Scenario for BadAssert {
    type Ctx = Events;
    type Error = TestErr;
    type Output = u8;
    type Setup = ();
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Round;

    async fn run(ctx: &Self::Ctx, _task: &TaskIndex, _setup: &()) -> Result<u8, Self::Error> {
        ctx.push("run");
        Ok(7)
    }

    async fn validate(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &(),
        _output: &u8,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    async fn teardown(
        ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &(),
        result: Option<Result<&u8, &TestErr>>,
    ) -> Result<(), Self::Error> {
        let carried = result.is_some_and(|r| r.is_ok());
        ctx.push(format!("teardown:ok_carried={carried}"));
        Ok(())
    }
}

#[test]
fn teardown_still_runs_when_validate_fails() {
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<BadAssert>::new(runner_config(RunMode::Times(1), None, 1));
        runner.run(&ctx).await
    });

    assert_eq!(ctx.drain(), vec!["run", "teardown:ok_carried=true"]);
    assert_eq!(recorder.validate_failures, 1);
    assert_eq!(recorder.teardown_success, 1);
    assert_eq!(recorder.error_map.get("validate").copied(), Some(1));
}

// ---------- 任务级收尾 ----------

/// 任务级收尾:每任务一次,`result` 恒为 `None`,位于所有轮次之后。
#[derive(Default)]
struct TaskTeardown;

static TASK_TEARDOWN_CALLS: AtomicU64 = AtomicU64::new(0);

impl Scenario for TaskTeardown {
    type Ctx = Events;
    type Error = TestErr;
    type Output = ();
    type Setup = u32;
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Task;

    async fn setup(ctx: &Self::Ctx, _task: &TaskIndex) -> Result<u32, Self::Error> {
        ctx.push("setup");
        Ok(11)
    }

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &u32) -> Result<(), Self::Error> {
        ctx.push(format!("run:{}", task.round));
        Ok(())
    }

    async fn teardown(
        ctx: &Self::Ctx,
        _task: &TaskIndex,
        setup: &u32,
        result: Option<Result<&(), &TestErr>>,
    ) -> Result<(), Self::Error> {
        TASK_TEARDOWN_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ctx.push(format!(
            "teardown:setup={setup},result_is_none={}",
            result.is_none()
        ));
        Ok(())
    }
}

#[test]
fn task_teardown_runs_once_after_all_rounds() {
    TASK_TEARDOWN_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<TaskTeardown>::new(runner_config(RunMode::Times(3), None, 1));
        runner.run(&ctx).await
    });

    assert_eq!(
        ctx.drain(),
        vec![
            "setup",
            "run:0",
            "run:1",
            "run:2",
            "teardown:setup=11,result_is_none=true"
        ],
    );
    assert_eq!(
        TASK_TEARDOWN_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(recorder.teardown_success, 1);
}

/// 0 轮次的任务同样收尾(保证任务级 setup 建立的会话被拆解)。
#[test]
fn task_teardown_runs_even_without_rounds() {
    TASK_TEARDOWN_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<TaskTeardown>::new(runner_config(RunMode::Times(0), None, 1));
        runner.run(&ctx).await
    });

    assert_eq!(
        ctx.drain(),
        vec!["setup", "teardown:setup=11,result_is_none=true"]
    );
    assert_eq!(recorder.teardown_success, 1);
}

/// `SetupMode::Round` + `TeardownMode::Task`:任务级收尾拿到最后一次 setup。
#[derive(Default)]
struct RoundSetupTaskTeardown;

impl Scenario for RoundSetupTaskTeardown {
    type Ctx = Events;
    type Error = TestErr;
    type Output = ();
    type Setup = usize;
    const SETUP_MODE: SetupMode = SetupMode::Round;
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Task;

    async fn setup(ctx: &Self::Ctx, task: &TaskIndex) -> Result<usize, Self::Error> {
        ctx.push(format!("setup:{}", task.round));
        Ok(task.round)
    }

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, setup: &usize) -> Result<(), Self::Error> {
        ctx.push(format!("run:{}:setup={setup}", task.round));
        Ok(())
    }

    async fn teardown(
        ctx: &Self::Ctx,
        _task: &TaskIndex,
        setup: &usize,
        result: Option<Result<&(), &TestErr>>,
    ) -> Result<(), Self::Error> {
        ctx.push(format!("teardown:setup={setup},none={}", result.is_none()));
        Ok(())
    }
}

#[test]
fn round_setup_with_task_teardown_uses_last_setup() {
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<RoundSetupTaskTeardown>::new(runner_config(
            RunMode::Times(3),
            None,
            1,
        ));
        runner.run(&ctx).await
    });

    assert_eq!(
        ctx.drain(),
        vec![
            "setup:0",
            "run:0:setup=0",
            "setup:1",
            "run:1:setup=1",
            "setup:2",
            "run:2:setup=2",
            "teardown:setup=2,none=true",
        ],
    );
    assert_eq!(recorder.teardown_success, 1);
}

// ---------- 失败口径:只记账,不中止、不进 failures ----------

/// 收尾一直失败:不中止任务、不计入 failures/timeouts、不影响通过率。
#[derive(Default)]
struct FailingTeardown;

impl Scenario for FailingTeardown {
    type Ctx = Events;
    type Error = TestErr;
    type Output = ();
    type Setup = ();
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Round;

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        ctx.push(format!("run:{}", task.round));
        Ok(())
    }

    async fn teardown(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &(),
        _result: Option<Result<&(), &TestErr>>,
    ) -> Result<(), Self::Error> {
        Err(TestErr::Cleanup)
    }
}

#[test]
fn teardown_failure_is_isolated_from_load_metrics() {
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner =
            ScenarioRunner::<FailingTeardown>::new(runner_config(RunMode::Times(3), None, 1));
        runner.run(&ctx).await
    });

    assert_eq!(ctx.drain().len(), 3, "收尾失败不影响后续轮次");
    assert_eq!(recorder.success, 3, "收尾失败不改 run 侧成功数");
    assert_eq!(recorder.validate_success, 3);
    assert_eq!(recorder.failures, 0, "收尾失败不进 failures");
    assert_eq!(recorder.timeouts, 0, "收尾失败不进 timeouts");
    assert_eq!(recorder.teardown_failures, 3);
    assert_eq!(recorder.teardown_success, 0);
    // 错误明细以 teardown: 前缀单列,不与 run 侧类目混淆
    assert_eq!(recorder.error_map.get("teardown:cleanup").copied(), Some(3));
    assert_eq!(recorder.error_map.get("cleanup"), None);
}

// ---------- 边界:setup 未成功则不收尾 ----------

/// setup 普通失败:任务中止,任务级收尾不执行。
#[derive(Default)]
struct SetupFails;

impl Scenario for SetupFails {
    type Ctx = Events;
    type Error = TestErr;
    type Output = ();
    type Setup = ();

    async fn setup(_ctx: &Self::Ctx, _task: &TaskIndex) -> Result<(), Self::Error> {
        Err(TestErr::Boom)
    }

    async fn run(_ctx: &Self::Ctx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn teardown(
        ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &(),
        _result: Option<Result<&(), &TestErr>>,
    ) -> Result<(), Self::Error> {
        ctx.push("teardown");
        Ok(())
    }
}

#[test]
fn task_teardown_is_skipped_when_setup_fails() {
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<SetupFails>::new(runner_config(RunMode::Times(2), None, 1));
        runner.run(&ctx).await
    });

    assert!(ctx.drain().is_empty(), "setup 失败不应触发收尾");
    assert_eq!(recorder.failures, 1);
    assert_eq!(recorder.teardown_success + recorder.teardown_failures, 0);
}

/// `SetupMode::Round` 且本轮 setup 失败:本轮不 run/不收尾。
#[derive(Default)]
struct RoundSetupFails;

impl Scenario for RoundSetupFails {
    type Ctx = Events;
    type Error = TestErr;
    type Output = ();
    type Setup = ();
    const SETUP_MODE: SetupMode = SetupMode::Round;
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Round;

    async fn setup(ctx: &Self::Ctx, task: &TaskIndex) -> Result<(), Self::Error> {
        if task.round == 0 {
            Err(TestErr::Boom)
        } else {
            ctx.push(format!("setup:{}", task.round));
            Ok(())
        }
    }

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        ctx.push(format!("run:{}", task.round));
        Ok(())
    }

    async fn teardown(
        ctx: &Self::Ctx,
        task: &TaskIndex,
        _setup: &(),
        _result: Option<Result<&(), &TestErr>>,
    ) -> Result<(), Self::Error> {
        ctx.push(format!("teardown:{}", task.round));
        Ok(())
    }
}

#[test]
fn round_teardown_is_skipped_when_round_setup_fails() {
    let ctx = Events::default();
    let recorder = rt().block_on(async {
        let runner =
            ScenarioRunner::<RoundSetupFails>::new(runner_config(RunMode::Times(2), None, 1));
        runner.run(&ctx).await
    });

    // 首轮 setup 普通失败即中止任务(不重试),因此只有 setup 失败记录
    assert!(ctx.drain().is_empty(), "本轮没有 run,也不应收尾");
    assert_eq!(recorder.failures, 1);
    assert_eq!(recorder.teardown_success, 0);
}

// ---------- 优雅关闭:收尾仍执行 ----------

/// 首轮 run 触发停止信号;轮级收尾与任务级收尾都在退出前完成。
#[derive(Default)]
struct CancelThenTeardown;

impl Scenario for CancelThenTeardown {
    type Ctx = Events;
    type Error = TestErr;
    type Output = ();
    type Setup = ();
    const TEARDOWN_MODE: TeardownMode = TeardownMode::Round;

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        ctx.push(format!("run:{}", task.round));
        ctx.fire_shutdown();
        Ok(())
    }

    async fn teardown(
        ctx: &Self::Ctx,
        task: &TaskIndex,
        _setup: &(),
        _result: Option<Result<&(), &TestErr>>,
    ) -> Result<(), Self::Error> {
        ctx.push(format!("teardown:{}", task.round));
        Ok(())
    }
}

#[test]
fn current_round_teardown_completes_after_shutdown() {
    let (sender, shutdown) = Shutdown::new();
    let ctx = Events::default();
    ctx.arm_shutdown(sender);

    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<CancelThenTeardown>::new(runner_config(
            RunMode::Times(5),
            Some(shutdown),
            1,
        ));
        runner.run(&ctx).await
    });

    assert_eq!(
        ctx.drain(),
        vec!["run:0", "teardown:0"],
        "当前轮收尾必须完成后才退出,后续轮次不再启动"
    );
    assert!(recorder.interrupted);
    assert_eq!(recorder.success, 1);
    assert_eq!(recorder.teardown_success, 1);
}

/// 任务级收尾在取消路径上同样执行。
#[derive(Default)]
struct CancelTaskTeardown;

impl Scenario for CancelTaskTeardown {
    type Ctx = Events;
    type Error = TestErr;
    type Output = ();
    type Setup = ();

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        ctx.push(format!("run:{}", task.round));
        ctx.fire_shutdown();
        Ok(())
    }

    async fn teardown(
        ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &(),
        result: Option<Result<&(), &TestErr>>,
    ) -> Result<(), Self::Error> {
        ctx.push(format!("teardown:none={}", result.is_none()));
        Ok(())
    }
}

#[test]
fn task_teardown_runs_on_shutdown_path() {
    let (sender, shutdown) = Shutdown::new();
    let ctx = Events::default();
    ctx.arm_shutdown(sender);

    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<CancelTaskTeardown>::new(runner_config(
            RunMode::Times(5),
            Some(shutdown),
            1,
        ));
        runner.run(&ctx).await
    });

    assert_eq!(ctx.drain(), vec!["run:0", "teardown:none=true"]);
    assert!(recorder.interrupted);
    assert_eq!(recorder.teardown_success, 1);
}
