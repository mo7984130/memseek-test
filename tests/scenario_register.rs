use std::borrow::Cow;

use memseek_test::{
    RunMode,
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

    async fn run(_ctx: &AuthContext) -> Result<(), Self::Error> {
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

    async fn run(_ctx: &AuthContext) -> Result<(), Self::Error> {
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

    async fn run(_ctx: &AuthContext) -> Result<u32, Self::Error> {
        Ok(42)
    }

    async fn validate(_ctx: &AuthContext, output: &u32) -> Result<bool, Self::Error> {
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

    async fn run(_ctx: &AuthContext) -> Result<(), Self::Error> {
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

    async fn run(_ctx: &AuthContext) -> Result<u32, Self::Error> {
        Ok(43)
    }

    async fn validate(_ctx: &AuthContext, output: &u32) -> Result<bool, Self::Error> {
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
