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
    scenario::Scenario,
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
    type Preset = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _preset: &()) -> Result<(), Self::Error> {
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
    type Preset = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _preset: &()) -> Result<(), Self::Error> {
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
    type Preset = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _preset: &()) -> Result<u32, Self::Error> {
        Ok(42)
    }

    async fn validate(
        _ctx: &AuthContext,
        _task: &TaskIndex,
        _preset: &(),
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
    type Preset = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _preset: &()) -> Result<(), Self::Error> {
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
    type Preset = ();

    async fn run(_ctx: &AuthContext, _task: &TaskIndex, _preset: &()) -> Result<u32, Self::Error> {
        Ok(43)
    }

    async fn validate(
        _ctx: &AuthContext,
        _task: &TaskIndex,
        _preset: &(),
        output: &u32,
    ) -> Result<bool, Self::Error> {
        Ok(*output == 42)
    }
}

register_scenario!(BadOutputScenario);

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
        // run 成功,但 validate 断言失败
        assert_eq!(report.success, 1);
        assert_eq!(report.validate_success, 0);
        assert_eq!(report.validate_failures, 1);
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
    type Preset = ();

    async fn run(ctx: &Self::Ctx, task: &TaskIndex, _preset: &()) -> Result<usize, Self::Error> {
        ctx.seen.lock().unwrap().push(task.index);
        Ok(task.index)
    }

    async fn validate(
        _ctx: &Self::Ctx,
        task: &TaskIndex,
        _preset: &(),
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

/// preset 阶段:每个任务执行一次,产出注入每轮 run/validate。
struct PresetCtx {
    events: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
struct PresetScenario;

impl Scenario for PresetScenario {
    type Ctx = PresetCtx;
    type Error = TestError;
    type Output = String;
    type Preset = String;

    async fn preset(ctx: &PresetCtx, task: &TaskIndex) -> Result<String, Self::Error> {
        ctx.events
            .lock()
            .unwrap()
            .push(format!("preset-{}", task.index));
        Ok(format!("token-{}", task.index))
    }

    async fn run(
        ctx: &PresetCtx,
        task: &TaskIndex,
        preset: &String,
    ) -> Result<String, Self::Error> {
        ctx.events
            .lock()
            .unwrap()
            .push(format!("run-{}", task.index));
        Ok(format!("{}-{}", preset, task.index))
    }

    async fn validate(
        ctx: &PresetCtx,
        task: &TaskIndex,
        preset: &String,
        output: &String,
    ) -> Result<bool, Self::Error> {
        ctx.events
            .lock()
            .unwrap()
            .push(format!("validate-{}", task.index));
        Ok(output == &format!("{}-{}", preset, task.index))
    }
}

register_scenario!(PresetScenario);

#[test]
fn preset_runs_once_before_each_task_and_feeds_run() {
    let ctx = PresetCtx {
        events: Arc::new(Mutex::new(Vec::new())),
    };
    let manager = ScenarioManager::new(ManagerConfig::new(2).with_run_mode(RunMode::Times(4)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<PresetCtx>("PresetScenario", &ctx)
            .await
            .expect("scenario should be found");
        // run 每轮都拿到了 preset 注入的 token,validate 断言全部通过
        assert_eq!(report.success, 4);
        assert_eq!(report.validate_success, 4);
        assert_eq!(report.validate_failures, 0);
    });

    let events = ctx.events.lock().unwrap().clone();
    for i in 0..2 {
        let tag = |name: &str| format!("{name}-{i}");
        // 每个任务 preset 恰好执行一次
        assert_eq!(
            events.iter().filter(|e| **e == tag("preset")).count(),
            1,
            "preset 应每个任务恰好一次: {events:?}"
        );
        // preset 必须发生在该任务首次 run 之前
        let first_run = events.iter().position(|e| *e == tag("run")).unwrap();
        assert!(
            events[..first_run].contains(&tag("preset")),
            "preset 应发生在该任务首次 run 之前: {events:?}"
        );
    }
}

/// preset 返回 Err:任务中止,不进入 run 循环,记一次失败。
struct PresetFailCtx {
    ran: Arc<Mutex<bool>>,
}

#[derive(Default)]
struct PresetFailScenario;

impl Scenario for PresetFailScenario {
    type Ctx = PresetFailCtx;
    type Error = TestError;
    type Output = ();
    type Preset = ();

    async fn preset(_ctx: &PresetFailCtx, _task: &TaskIndex) -> Result<(), Self::Error> {
        Err(TestError)
    }

    async fn run(ctx: &PresetFailCtx, _task: &TaskIndex, _preset: &()) -> Result<(), Self::Error> {
        *ctx.ran.lock().unwrap() = true;
        Ok(())
    }
}

register_scenario!(PresetFailScenario);

#[test]
fn preset_failure_aborts_task_and_counts_one_failure() {
    let ctx = PresetFailCtx {
        ran: Arc::new(Mutex::new(false)),
    };
    let manager = ScenarioManager::new(ManagerConfig::new(1).with_run_mode(RunMode::Times(3)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<PresetFailCtx>("PresetFailScenario", &ctx)
            .await
            .expect("scenario should be found");
        // preset 失败记 1 次失败,run 循环一轮都没执行
        assert_eq!(report.success, 0);
        assert_eq!(report.failures, 1);
        assert_eq!(report.times, 0);
    });

    assert!(!*ctx.ran.lock().unwrap(), "preset 失败后不应进入 run");
}

/// round(全局运行编号):preset 取号后,该任务内三阶段共享同一编号,且全局唯一。
struct RoundCtx {
    records: Arc<Mutex<Vec<(String, String, usize)>>>, // (阶段, 任务, round)
}

#[derive(Default)]
struct RoundScenario;

impl Scenario for RoundScenario {
    type Ctx = RoundCtx;
    type Error = TestError;
    type Output = usize;
    type Preset = String;

    async fn preset(ctx: &RoundCtx, task: &TaskIndex) -> Result<String, Self::Error> {
        ctx.records
            .lock()
            .unwrap()
            .push(("preset".into(), format!("t{}", task.index), task.round));
        Ok(format!("r{}", task.round))
    }

    async fn run(ctx: &RoundCtx, task: &TaskIndex, preset: &String) -> Result<usize, Self::Error> {
        ctx.records
            .lock()
            .unwrap()
            .push(("run".into(), format!("t{}", task.index), task.round));
        assert_eq!(preset, &format!("r{}", task.round));
        Ok(task.round)
    }

    async fn validate(
        ctx: &RoundCtx,
        task: &TaskIndex,
        _preset: &String,
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

register_scenario!(RoundScenario);

#[test]
fn round_is_global_and_shared_across_phases() {
    let ctx = RoundCtx {
        records: Arc::new(Mutex::new(Vec::new())),
    };
    // 并发 2 × 每任务 2 轮
    let manager = ScenarioManager::new(ManagerConfig::new(2).with_run_mode(RunMode::Times(4)));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let report = manager
            .run_one::<RoundCtx>("RoundScenario", &ctx)
            .await
            .expect("scenario should be found");
        assert_eq!(report.success, 4);
        assert_eq!(report.validate_success, 4);
    });

    let records = ctx.records.lock().unwrap().clone();
    let rounds: Vec<usize> = records.iter().map(|r| r.2).collect();
    // 全局编号:两个任务各取一个,且连续不重复
    assert_eq!(
        rounds
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2
    );
    assert!(rounds.iter().all(|r| *r < 2), "rounds = {rounds:?}");

    // 每个任务:preset 恰好 1 次、run 2 次、validate 2 次,三阶段 round 相同
    for i in 0..2 {
        let tag = format!("t{i}");
        let task_events: Vec<_> = records.iter().filter(|(_, t, _)| *t == tag).collect();
        let phases: Vec<&str> = task_events.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(phases.iter().filter(|p| **p == "preset").count(), 1);
        assert_eq!(phases.iter().filter(|p| **p == "run").count(), 2);
        assert_eq!(phases.iter().filter(|p| **p == "validate").count(), 2);
        let task_rounds: Vec<usize> = task_events.iter().map(|e| e.2).collect();
        assert!(
            task_rounds.iter().all(|r| *r == task_rounds[0]),
            "任务 {tag} 三阶段 round 应一致: {task_rounds:?}"
        );
    }

    // 两个任务取到的是不同的 round(全局唯一)
    let r0 = records.iter().find(|e| e.1 == "t0").unwrap().2;
    let r1 = records.iter().find(|e| e.1 == "t1").unwrap().2;
    assert_ne!(r0, r1);
}
