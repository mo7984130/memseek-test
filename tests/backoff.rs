//! 超时退避与时序:软失败(超时)不计入 failures,超时后等待退避再发下一轮。

use std::{
    borrow::Cow,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use memseek_test::{
    BackoffConfig, RunMode, Shutdown,
    error::ScenarioError,
    runner::{RunnerConfig, ScenarioRunner, TaskIndex},
    scenario::Scenario,
};

/// 可标记超时的测试错误。
#[derive(Debug)]
enum TestErr {
    Timeout,
    Boom,
}

impl ScenarioError for TestErr {
    fn kind(&self) -> Cow<'static, str> {
        Cow::Borrowed("flaky")
    }

    fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }
}

/// 第一轮超时、之后成功:验证归因与退避后的恢复。
#[derive(Default)]
struct FlakyFirst;

static FLAKY_FIRST_CALLS: AtomicU64 = AtomicU64::new(0);

impl Scenario for FlakyFirst {
    type Ctx = ();
    type Error = TestErr;
    type Output = ();
    type Setup = ();

    async fn run(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &Self::Setup,
    ) -> Result<Self::Output, Self::Error> {
        if FLAKY_FIRST_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(TestErr::Timeout)
        } else {
            Ok(())
        }
    }
}

/// 每轮都超时:验证停止信号能打断退避等待。
#[derive(Default)]
struct FlakyAlways;

static FLAKY_ALWAYS_CALLS: AtomicU64 = AtomicU64::new(0);

impl Scenario for FlakyAlways {
    type Ctx = ();
    type Error = TestErr;
    type Output = ();
    type Setup = ();

    async fn run(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &Self::Setup,
    ) -> Result<Self::Output, Self::Error> {
        FLAKY_ALWAYS_CALLS.fetch_add(1, Ordering::SeqCst);
        Err(TestErr::Timeout)
    }
}

/// 独立于 `FlakyFirst` 的计数器:避免并行测试间共享静态状态。
#[derive(Default)]
struct FlakyFirstNoBackoff;

static FLAKY_FIRST_NO_BACKOFF_CALLS: AtomicU64 = AtomicU64::new(0);

impl Scenario for FlakyFirstNoBackoff {
    type Ctx = ();
    type Error = TestErr;
    type Output = ();
    type Setup = ();

    async fn run(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &Self::Setup,
    ) -> Result<Self::Output, Self::Error> {
        if FLAKY_FIRST_NO_BACKOFF_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(TestErr::Timeout)
        } else {
            Ok(())
        }
    }
}

/// setup 首次超时、之后成功:验证 setup 纳入退避重试。
#[derive(Default)]
struct SetupFlaky;

static SETUP_FLAKY_CALLS: AtomicU64 = AtomicU64::new(0);

impl Scenario for SetupFlaky {
    type Ctx = ();
    type Error = TestErr;
    type Output = ();
    type Setup = ();

    async fn setup(_ctx: &Self::Ctx, _task: &TaskIndex) -> Result<Self::Setup, Self::Error> {
        if SETUP_FLAKY_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(TestErr::Timeout)
        } else {
            Ok(())
        }
    }

    async fn run(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &Self::Setup,
    ) -> Result<Self::Output, Self::Error> {
        Ok(())
    }
}

/// setup 普通失败:应中止任务(不重试)。
#[derive(Default)]
struct SetupHardFail;

impl Scenario for SetupHardFail {
    type Ctx = ();
    type Error = TestErr;
    type Output = ();
    type Setup = ();

    async fn setup(_ctx: &Self::Ctx, _task: &TaskIndex) -> Result<Self::Setup, Self::Error> {
        Err(TestErr::Boom)
    }

    async fn run(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &Self::Setup,
    ) -> Result<Self::Output, Self::Error> {
        Ok(())
    }
}

/// setup 超时但未配置退避:保持旧语义,中止任务。
#[derive(Default)]
struct SetupTimeoutNoBackoff;

impl Scenario for SetupTimeoutNoBackoff {
    type Ctx = ();
    type Error = TestErr;
    type Output = ();
    type Setup = ();

    async fn setup(_ctx: &Self::Ctx, _task: &TaskIndex) -> Result<Self::Setup, Self::Error> {
        Err(TestErr::Timeout)
    }

    async fn run(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _setup: &Self::Setup,
    ) -> Result<Self::Output, Self::Error> {
        Ok(())
    }
}

fn runner_config(
    mode: RunMode,
    shutdown: Option<Shutdown>,
    backoff: Option<BackoffConfig>,
) -> RunnerConfig {
    RunnerConfig {
        mode,
        task_index: 0,
        task_total: 1,
        shutdown,
        backoff,
        #[cfg(feature = "tui")]
        progress: None,
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn timeout_is_not_a_failure_and_run_continues() {
    FLAKY_FIRST_CALLS.store(0, Ordering::SeqCst);
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<FlakyFirst>::new(runner_config(
            RunMode::Times(2),
            None,
            Some(BackoffConfig::new(
                Duration::from_millis(10),
                Duration::from_millis(100),
                2.0,
            )),
        ));
        runner.run(&()).await
    });

    assert_eq!(recorder.success, 1, "第二轮应成功");
    assert_eq!(recorder.failures, 0, "超时不加剧失败");
    assert_eq!(recorder.timeouts, 1, "超时单独计数");
}

#[test]
fn backoff_wait_is_interrupted_by_shutdown() {
    FLAKY_ALWAYS_CALLS.store(0, Ordering::SeqCst);
    let recorder = rt().block_on(async {
        let (tx, shutdown) = Shutdown::new();
        let runner = ScenarioRunner::<FlakyAlways>::new(runner_config(
            RunMode::Times(1000),
            Some(shutdown),
            Some(BackoffConfig::new(
                Duration::from_secs(5),
                Duration::from_secs(5),
                2.0,
            )),
        ));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            tx.cancel();
        });
        runner.run(&()).await
    });

    assert!(recorder.interrupted, "退避期间应能响应停止信号");
    assert_eq!(recorder.failures, 0);
    assert!(recorder.timeouts >= 1, "至少发生一轮超时");
}

#[test]
fn setup_timeout_backs_off_then_retries() {
    SETUP_FLAKY_CALLS.store(0, Ordering::SeqCst);
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<SetupFlaky>::new(runner_config(
            RunMode::Times(2),
            None,
            Some(BackoffConfig::new(
                Duration::from_millis(10),
                Duration::from_millis(100),
                2.0,
            )),
        ));
        runner.run(&()).await
    });

    assert_eq!(recorder.success, 2, "setup 重试成功后应跑满轮次");
    assert_eq!(recorder.failures, 0);
    assert_eq!(recorder.timeouts, 1, "setup 超时计入 timeouts");
}

#[test]
fn setup_plain_failure_aborts_task() {
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<SetupHardFail>::new(runner_config(
            RunMode::Times(2),
            None,
            Some(BackoffConfig::default()),
        ));
        runner.run(&()).await
    });

    assert_eq!(recorder.success, 0, "setup 失败不应进入 run");
    assert_eq!(recorder.failures, 1, "普通失败计为失败且不重试");
    assert_eq!(recorder.timeouts, 0);
}

#[test]
fn setup_timeout_without_backoff_aborts_task() {
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<SetupTimeoutNoBackoff>::new(runner_config(
            RunMode::Times(2),
            None,
            None, // 未配置退避:保持旧语义
        ));
        runner.run(&()).await
    });

    assert_eq!(recorder.success, 0);
    assert_eq!(recorder.failures, 0);
    assert_eq!(recorder.timeouts, 1);
}

#[test]
fn timeout_without_backoff_immediately_continues() {
    FLAKY_FIRST_NO_BACKOFF_CALLS.store(0, Ordering::SeqCst);
    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<FlakyFirstNoBackoff>::new(runner_config(
            RunMode::Times(2),
            None,
            None, // 未配置退避:超时后立即下一轮
        ));
        runner.run(&()).await
    });

    assert_eq!(recorder.success, 1);
    assert_eq!(recorder.timeouts, 1);
}

#[test]
fn plain_failure_is_still_a_failure() {
    #[derive(Default)]
    struct HardFail;
    impl Scenario for HardFail {
        type Ctx = ();
        type Error = TestErr;
        type Output = ();
        type Setup = ();

        async fn run(
            _ctx: &Self::Ctx,
            _task: &TaskIndex,
            _setup: &Self::Setup,
        ) -> Result<Self::Output, Self::Error> {
            Err(TestErr::Boom)
        }
    }

    let recorder = rt().block_on(async {
        let runner = ScenarioRunner::<HardFail>::new(runner_config(RunMode::Times(1), None, None));
        runner.run(&()).await
    });

    assert_eq!(recorder.success, 0);
    assert_eq!(recorder.failures, 1, "普通失败仍计为失败");
    assert_eq!(recorder.timeouts, 0, "普通失败不计为超时");
}
