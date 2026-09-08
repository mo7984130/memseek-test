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

    async fn run(_ctx: &AuthContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

register_scenario!(
    NamedScenario,
    name = "custom-name",
    mode = RunMode::Times(1)
);

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
