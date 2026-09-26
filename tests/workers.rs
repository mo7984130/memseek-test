use std::{borrow::Cow, collections::HashSet, sync::Mutex, thread::ThreadId, time::Duration};

use memseek_test::{
    RunMode, ScenarioReport, TaskIndex,
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

/// 记录每轮由哪个线程驱动、拿到哪些任务身份,用于验证 `workers` 分片。
struct ProbeCtx {
    threads: Mutex<HashSet<ThreadId>>,
    tasks: Mutex<HashSet<(usize, usize)>>,
}

impl ProbeCtx {
    fn new() -> Self {
        Self {
            threads: Mutex::new(HashSet::new()),
            tasks: Mutex::new(HashSet::new()),
        }
    }

    fn threads(&self) -> HashSet<ThreadId> {
        self.threads.lock().unwrap().clone()
    }

    fn tasks(&self) -> HashSet<(usize, usize)> {
        self.tasks.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct WorkerProbe;

impl Scenario for WorkerProbe {
    type Ctx = ProbeCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        ctx.threads
            .lock()
            .unwrap()
            .insert(std::thread::current().id());
        ctx.tasks.lock().unwrap().insert((task.index, task.total));
        // 走一次定时器:验证分片 runtime 的 time driver 可用(退避依赖它)
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(())
    }
}

register_scenario!(WorkerProbe);

/// 在 `run` 中直接 panic,用于验证分片内的 panic 传播。
#[derive(Default)]
struct PanicProbe;

impl Scenario for PanicProbe {
    type Ctx = ProbeCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(_ctx: &Self::Ctx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        panic!("boom from shard");
    }
}

register_scenario!(PanicProbe);

fn run_probe(ctx: &ProbeCtx, manager: &ScenarioManager) -> ScenarioReport {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        manager
            .run_one::<ProbeCtx>("WorkerProbe", ctx)
            .await
            .expect("scenario should be found")
    })
}

/// 默认(未设置 workers):与历史行为一致,全部任务在当前 runtime 单线程协作调度。
#[test]
fn default_single_worker_keeps_inline_scheduling() {
    let ctx = ProbeCtx::new();
    let manager = ScenarioManager::new(ManagerConfig::new(4).with_run_mode(RunMode::Times(8)));

    let report = run_probe(&ctx, &manager);

    assert_eq!(report.times, 8);
    assert_eq!(report.success, 8);
    assert_eq!(report.validate_success, 8);
    assert_eq!(ctx.threads().len(), 1, "单 worker 应只有一个驱动线程");
    assert_eq!(
        ctx.tasks(),
        HashSet::from([(0, 4), (1, 4), (2, 4), (3, 4)]),
        "任务身份仍是全局编号"
    );
}

/// `workers = 2`:任务分片到 2 个自建线程,每线程一个 current_thread runtime。
#[test]
fn workers_split_tasks_across_threads() {
    let ctx = ProbeCtx::new();
    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Times(8))
            .with_workers(2),
    );

    let report = run_probe(&ctx, &manager);

    assert_eq!(report.times, 8, "分片不改变总轮次");
    assert_eq!(report.success, 8);
    assert_eq!(report.concurrency, 4, "分片不改变报告并发度");
    assert_eq!(ctx.threads().len(), 2, "2 个分片应由 2 个线程驱动");
    assert_eq!(ctx.tasks(), HashSet::from([(0, 4), (1, 4), (2, 4), (3, 4)]));
}

/// `workers` 超过并发度时按并发度裁剪。
#[test]
fn workers_are_clamped_to_concurrency() {
    let ctx = ProbeCtx::new();
    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Times(8))
            .with_workers(99),
    );

    let report = run_probe(&ctx, &manager);

    assert_eq!(report.times, 8);
    assert_eq!(report.success, 8);
    assert_eq!(ctx.threads().len(), 4, "裁剪后应为 4 个分片线程");
}

/// `Duration` 模式同样支持分片:轮次循环与定时器在各分片内正常工作。
#[test]
fn duration_mode_runs_on_workers() {
    let ctx = ProbeCtx::new();
    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Duration(Duration::from_millis(60)))
            .with_workers(2),
    );

    let report = run_probe(&ctx, &manager);

    assert!(report.times > 0, "时长模式应产生轮次");
    assert_eq!(report.success, report.times);
    assert_eq!(report.validate_success, report.times);
    assert_eq!(ctx.threads().len(), 2);
}

/// 分片内 panic:等其余分片收尾后原样抛给调用方(不悬挂、不吞掉)。
#[test]
fn shard_panic_propagates_to_caller() {
    let ctx = ProbeCtx::new();
    let manager = ScenarioManager::new(
        ManagerConfig::new(4)
            .with_run_mode(RunMode::Times(4))
            .with_workers(2),
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // 分片线程上的 panic 会先由默认钩子打印,这里静音以保持测试输出干净
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(async { manager.run_one::<ProbeCtx>("PanicProbe", &ctx).await });
    }));
    std::panic::set_hook(previous);

    assert!(caught.is_err(), "分片 panic 应传播到调用方");
}
