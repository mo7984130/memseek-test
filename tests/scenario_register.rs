use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};

use memseek_test::{
    RunMode, TaskIndex,
    error::ScenarioError,
    manager::{ManagerConfig, ScenarioManager},
    register_scenario,
    registry::ScenarioRegistry,
    scenario::{Scenario, SetupMode},
};

#[derive(Debug)]
struct TestError;

impl ScenarioError for TestError {
    fn kind(&self) -> Cow<'static, str> {
        Cow::Borrowed("test")
    }
}

struct AuthContext;

/// 最小场景:只实现 `run`,`name()` / `validate()` 走默认实现。
#[derive(Default)]
struct HelloScenario;

impl Scenario for HelloScenario {
    type Ctx = AuthContext;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        Ok(())
    }
}

// 默认匹配名(类型简单名)+ 默认 mode(None)
register_scenario!(HelloScenario);

/// 覆盖匹配名和默认执行模式。
#[derive(Default)]
struct NamedScenario;

impl Scenario for NamedScenario {
    type Ctx = AuthContext;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        Ok(())
    }
}

register_scenario!(
    NamedScenario,
    name = "custom-name",
    mode = RunMode::Times(1)
);

/// `run` 产出业务数据,`validate` 基于本轮结果做断言。
#[derive(Default)]
struct OutputScenario;

impl Scenario for OutputScenario {
    type Ctx = AuthContext;
    type Error = TestError;
    type Output = u32;
    type Setup = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _setup: &()) -> Result<u32, Self::Error> {
        Ok(42)
    }

    async fn validate(
        _ctx: &AuthContext,
        _task: &TaskIndex,
        _setup: &(),
        output: &u32,
    ) -> Result<bool, Self::Error> {
        Ok(*output == 42)
    }
}

register_scenario!(OutputScenario);

#[test]
fn default_match_name_is_type_name() {
    let names = ScenarioRegistry::names::<AuthContext>();
    assert!(names.contains(&"HelloScenario"), "names = {names:?}");
}

#[test]
fn custom_name_and_mode_are_registered() {
    let entry = ScenarioRegistry::find::<AuthContext>("custom-name")
        .expect("custom-name should be registered");
    assert!(entry.config.run_mode.is_some());
}

#[test]
fn manager_runs_registered_scenario() {
    let ctx = AuthContext;
    let manager = ScenarioManager::new(ManagerConfig::new(1).with_run_mode(RunMode::Times(1)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<AuthContext>("HelloScenario", &ctx)
            .await
            .expect("scenario should be found");
        assert_eq!(report.name, "HelloScenario");
        assert_eq!(report.times, 1);
        assert_eq!(report.success, 1);
    });
}

#[test]
fn validate_receives_run_output() {
    let ctx = AuthContext;
    let manager = ScenarioManager::new(ManagerConfig::new(1).with_run_mode(RunMode::Times(1)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<AuthContext>("OutputScenario", &ctx)
            .await
            .expect("scenario should be found");
        // validate 拿到了 run 的产出 42 并断言通过
        assert_eq!(report.validate_success, 1);
        assert_eq!(report.validate_failures, 0);
    });
}

/// `run` 恒失败,验证失败时跳过 validate。
#[derive(Default)]
struct FailingScenario;

impl Scenario for FailingScenario {
    type Ctx = AuthContext;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        Err(TestError)
    }
}

register_scenario!(FailingScenario);

#[test]
fn run_failure_skips_validate() {
    let ctx = AuthContext;
    let manager = ScenarioManager::new(ManagerConfig::new(1).with_run_mode(RunMode::Times(1)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<AuthContext>("FailingScenario", &ctx)
            .await
            .expect("scenario should be found");
        // run 失败计入 failures,validate 没有被调用
        assert_eq!(report.success, 0);
        assert_eq!(report.failures, 1);
        assert_eq!(report.validate_success, 0);
        assert_eq!(report.validate_failures, 0);
    });
}

/// `run` 成功但产出不满足校验,计入一次 validate 失败。
#[derive(Default)]
struct BadOutputScenario;

impl Scenario for BadOutputScenario {
    type Ctx = AuthContext;
    type Error = TestError;
    type Output = u32;
    type Setup = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _setup: &()) -> Result<u32, Self::Error> {
        Ok(43)
    }

    async fn validate(
        _ctx: &AuthContext,
        _task: &TaskIndex,
        _setup: &(),
        output: &u32,
    ) -> Result<bool, Self::Error> {
        Ok(*output == 42)
    }
}

register_scenario!(BadOutputScenario);

/// `run` 成功且产出满足校验,但 validate 自身返回 `Err`(抛错),
/// 同样计入一次 validate 失败,并以 `Error::kind()` 作为错误类目。
#[derive(Default)]
struct ValidateErrorScenario;

impl Scenario for ValidateErrorScenario {
    type Ctx = AuthContext;
    type Error = TestError;
    type Output = u32;
    type Setup = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _setup: &()) -> Result<u32, Self::Error> {
        Ok(42)
    }

    async fn validate(
        _ctx: &AuthContext,
        _task: &TaskIndex,
        _setup: &(),
        _output: &u32,
    ) -> Result<bool, Self::Error> {
        Err(TestError)
    }
}

register_scenario!(ValidateErrorScenario);

#[test]
fn validate_failure_is_recorded() {
    let ctx = AuthContext;
    let manager = ScenarioManager::new(ManagerConfig::new(1).with_run_mode(RunMode::Times(1)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<AuthContext>("BadOutputScenario", &ctx)
            .await
            .expect("scenario should be found");
        // run 成功,但 validate 断言失败(Ok(false) -> 统一类目 "validate")
        assert_eq!(report.success, 1);
        assert_eq!(report.validate_success, 0);
        assert_eq!(report.validate_failures, 1);
        assert_eq!(report.error_map.get("validate"), Some(&1));
    });
}

#[test]
fn validate_error_is_recorded_with_kind() {
    let ctx = AuthContext;
    let manager = ScenarioManager::new(ManagerConfig::new(1).with_run_mode(RunMode::Times(1)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<AuthContext>("ValidateErrorScenario", &ctx)
            .await
            .expect("scenario should be found");
        // validate 返回 Err:以 `Error::kind()` 计入错误明细
        assert_eq!(report.validate_failures, 1);
        assert_eq!(report.error_map.get("test"), Some(&1));
    });
}

/// 每个并发任务收到唯一的 `task.index`,可用于账号等参数化。
struct IndexCtx {
    seen: Arc<Mutex<Vec<usize>>>,
}

#[derive(Default)]
struct IndexScenario;

impl Scenario for IndexScenario {
    type Ctx = IndexCtx;
    type Error = TestError;
    type Output = usize;
    type Setup = ();

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _setup: &()) -> Result<usize, Self::Error> {
        ctx.seen.lock().unwrap().push(task.index);
        Ok(task.index)
    }

    async fn validate(
        _ctx: &Self::Ctx,
        task: &TaskIndex,
        _setup: &(),
        output: &usize,
    ) -> Result<bool, Self::Error> {
        Ok(*output == task.index)
    }
}

register_scenario!(IndexScenario);

#[test]
fn task_index_is_assigned_per_concurrent_task() {
    let ctx = IndexCtx {
        seen: Arc::new(Mutex::new(Vec::new())),
    };
    let manager = ScenarioManager::new(ManagerConfig::new(4).with_run_mode(RunMode::Times(4)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<IndexCtx>("IndexScenario", &ctx)
            .await
            .expect("scenario should be found");
        assert_eq!(report.success, 4);
        assert_eq!(report.validate_success, 4);
    });

    // 4 个并发任务分别拿到 0..4 的唯一编号
    let mut seen = ctx.seen.lock().unwrap().clone();
    seen.sort_unstable();
    assert_eq!(seen, vec![0, 1, 2, 3]);
}

/// setup 阶段:每个任务执行一次,产出注入每轮 run/validate。
struct SetupCtx {
    events: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
struct SetupScenario;

impl Scenario for SetupScenario {
    type Ctx = SetupCtx;
    type Error = TestError;
    type Output = String;
    type Setup = String;

    async fn setup(ctx: &SetupCtx, task: &TaskIndex) -> Result<String, Self::Error> {
        ctx.events
            .lock()
            .unwrap()
            .push(format!("setup-{}", task.index));
        Ok(format!("token-{}", task.index))
    }

    async fn run(ctx: &SetupCtx, task: &TaskIndex, setup: &String) -> Result<String, Self::Error> {
        ctx.events
            .lock()
            .unwrap()
            .push(format!("run-{}", task.index));
        Ok(format!("{}-{}", setup, task.index))
    }

    async fn validate(
        ctx: &SetupCtx,
        task: &TaskIndex,
        setup: &String,
        output: &String,
    ) -> Result<bool, Self::Error> {
        ctx.events
            .lock()
            .unwrap()
            .push(format!("validate-{}", task.index));
        Ok(output == &format!("{}-{}", setup, task.index))
    }
}

register_scenario!(SetupScenario);

#[test]
fn setup_runs_once_before_each_task_and_feeds_run() {
    let ctx = SetupCtx {
        events: Arc::new(Mutex::new(Vec::new())),
    };
    let manager = ScenarioManager::new(ManagerConfig::new(2).with_run_mode(RunMode::Times(4)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<SetupCtx>("SetupScenario", &ctx)
            .await
            .expect("scenario should be found");
        // run 每轮都拿到了 setup 注入的 token,validate 断言全部通过
        assert_eq!(report.success, 4);
        assert_eq!(report.validate_success, 4);
        assert_eq!(report.validate_failures, 0);
    });

    let events = ctx.events.lock().unwrap().clone();
    for i in 0..2 {
        let tag = |name: &str| format!("{name}-{i}");
        // 每个任务 setup 恰好执行一次
        assert_eq!(
            events.iter().filter(|e| **e == tag("setup")).count(),
            1,
            "setup 应每个任务恰好一次: {events:?}"
        );
        // setup 必须发生在该任务首次 run 之前
        let first_run = events.iter().position(|e| *e == tag("run")).unwrap();
        assert!(
            events[..first_run].contains(&tag("setup")),
            "setup 应发生在该任务首次 run 之前: {events:?}"
        );
    }
}

/// setup 返回 Err:任务中止,不进入 run 循环,记一次失败。
struct SetupFailCtx {
    ran: Arc<Mutex<bool>>,
}

#[derive(Default)]
struct SetupFailScenario;

impl Scenario for SetupFailScenario {
    type Ctx = SetupFailCtx;
    type Error = TestError;
    type Output = ();
    type Setup = ();

    async fn setup(_ctx: &SetupFailCtx, _task: &TaskIndex) -> Result<(), Self::Error> {
        Err(TestError)
    }

    async fn run(ctx: &SetupFailCtx, _task: &TaskIndex, _setup: &()) -> Result<(), Self::Error> {
        *ctx.ran.lock().unwrap() = true;
        Ok(())
    }
}

register_scenario!(SetupFailScenario);

#[test]
fn setup_failure_aborts_task_and_counts_one_failure() {
    let ctx = SetupFailCtx {
        ran: Arc::new(Mutex::new(false)),
    };
    let manager = ScenarioManager::new(ManagerConfig::new(1).with_run_mode(RunMode::Times(3)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<SetupFailCtx>("SetupFailScenario", &ctx)
            .await
            .expect("scenario should be found");
        // setup 失败记 1 次失败,run 循环一轮都没执行
        assert_eq!(report.success, 0);
        assert_eq!(report.failures, 1);
        assert_eq!(report.times, 0);
    });

    assert!(!*ctx.ran.lock().unwrap(), "setup 失败后不应进入 run");
}

/// round(全局运行编号):每次执行(setup/run)取号,编号全局连续唯一。
struct RoundCtx {
    records: Arc<Mutex<Vec<(String, String, usize)>>>, // (阶段, 任务, round)
}

/// Task 模式(默认):setup 每任务一次,每轮 run 独立取号。
#[derive(Default)]
struct TaskModeScenario;

impl Scenario for TaskModeScenario {
    type Ctx = RoundCtx;
    type Error = TestError;
    type Output = usize;
    type Setup = String;

    async fn setup(ctx: &RoundCtx, task: &TaskIndex) -> Result<String, Self::Error> {
        ctx.records
            .lock()
            .unwrap()
            .push(("setup".into(), format!("t{}", task.index), task.round));
        Ok(format!("r{}", task.round))
    }

    async fn run(ctx: &RoundCtx, task: &TaskIndex, _setup: &String) -> Result<usize, Self::Error> {
        ctx.records
            .lock()
            .unwrap()
            .push(("run".into(), format!("t{}", task.index), task.round));
        Ok(task.round)
    }

    async fn validate(
        ctx: &RoundCtx,
        task: &TaskIndex,
        _setup: &String,
        output: &usize,
    ) -> Result<bool, Self::Error> {
        ctx.records.lock().unwrap().push((
            "validate".into(),
            format!("t{}", task.index),
            task.round,
        ));
        Ok(*output == task.round)
    }
}

register_scenario!(TaskModeScenario);

#[test]
fn task_mode_takes_one_round_per_run() {
    let ctx = RoundCtx {
        records: Arc::new(Mutex::new(Vec::new())),
    };
    // 并发 2 × 每任务 2 轮:round 为任务内轮次计数(0,1)
    let manager = ScenarioManager::new(ManagerConfig::new(2).with_run_mode(RunMode::Times(4)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<RoundCtx>("TaskModeScenario", &ctx)
            .await
            .expect("scenario should be found");
        assert_eq!(report.success, 4);
        assert_eq!(report.validate_success, 4);
    });

    let records = ctx.records.lock().unwrap().clone();

    // setup 每任务恰 1 次,round 固定 0(无轮次含义)
    let setups: Vec<_> = records.iter().filter(|e| e.0 == "setup").collect();
    assert_eq!(setups.len(), 2, "records = {records:?}");
    assert!(
        setups.iter().all(|e| e.2 == 0),
        "setup round 应为 0: {records:?}"
    );

    // 每任务 2 轮:run/validate 同号成对,round 为 0,1
    for i in 0..2 {
        let tag = format!("t{i}");
        let ev: Vec<_> = records.iter().filter(|(_, t, _)| *t == tag).collect();
        assert_eq!(ev.iter().filter(|e| e.0 == "run").count(), 2);
        assert_eq!(ev.iter().filter(|e| e.0 == "validate").count(), 2);
        for round in 0..2 {
            assert_eq!(
                ev.iter().filter(|e| e.0 == "run" && e.2 == round).count(),
                1,
                "任务 {tag} round {round} 的 run 应恰 1 次: {records:?}"
            );
            assert_eq!(
                ev.iter()
                    .filter(|e| e.0 == "validate" && e.2 == round)
                    .count(),
                1,
                "任务 {tag} round {round} 的 validate 应恰 1 次: {records:?}"
            );
        }
    }
}

/// Round 模式:setup 每轮执行,与该轮 run/validate 同一编号。
#[derive(Default)]
struct RoundSetupScenario;

impl Scenario for RoundSetupScenario {
    type Ctx = RoundCtx;
    type Error = TestError;
    type Output = usize;
    type Setup = String;

    const SETUP_MODE: SetupMode = SetupMode::Round;

    async fn setup(ctx: &RoundCtx, task: &TaskIndex) -> Result<String, Self::Error> {
        ctx.records
            .lock()
            .unwrap()
            .push(("setup".into(), format!("t{}", task.index), task.round));
        Ok(format!("r{}", task.round))
    }

    async fn run(ctx: &RoundCtx, task: &TaskIndex, setup: &String) -> Result<usize, Self::Error> {
        ctx.records
            .lock()
            .unwrap()
            .push(("run".into(), format!("t{}", task.index), task.round));
        assert_eq!(setup, &format!("r{}", task.round));
        Ok(task.round)
    }

    async fn validate(
        ctx: &RoundCtx,
        task: &TaskIndex,
        _setup: &String,
        output: &usize,
    ) -> Result<bool, Self::Error> {
        ctx.records.lock().unwrap().push((
            "validate".into(),
            format!("t{}", task.index),
            task.round,
        ));
        Ok(*output == task.round)
    }
}

register_scenario!(RoundSetupScenario);

#[test]
fn round_mode_runs_setup_every_round_with_same_number() {
    let ctx = RoundCtx {
        records: Arc::new(Mutex::new(Vec::new())),
    };
    // 并发 2 × 每任务 2 轮 = 4 次 setup:round 为任务内轮次(0,1)
    let manager = ScenarioManager::new(ManagerConfig::new(2).with_run_mode(RunMode::Times(4)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<RoundCtx>("RoundSetupScenario", &ctx)
            .await
            .expect("scenario should be found");
        assert_eq!(report.success, 4);
        assert_eq!(report.validate_success, 4);
    });

    let records = ctx.records.lock().unwrap().clone();

    // setup 每轮执行:共 4 次;每任务每轮 setup/run/validate 恰好同号各一次
    assert_eq!(records.iter().filter(|e| e.0 == "setup").count(), 4);
    for i in 0..2 {
        let tag = format!("t{i}");
        for round in 0..2 {
            for phase in ["setup", "run", "validate"] {
                assert_eq!(
                    records
                        .iter()
                        .filter(|e| e.0 == phase && e.1 == tag && e.2 == round)
                        .count(),
                    1,
                    "任务 {tag} round {round} 的 {phase} 应恰 1 次: {records:?}"
                );
            }
        }
    }
}
