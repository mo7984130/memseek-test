use crate::error::ScenarioError;

pub trait Scenario: Default + Send + Sync + 'static {
    type Ctx: Send + Sync;
    type Error: ScenarioError;

    /// 场景展示名。
    ///
    /// 默认返回完整类型路径(如 `my_mod::HelloScenario`)。
    /// 注意:注册进 registry 的**匹配名**由 `register_scenario!` 生成,
    /// 默认是类型简单名(可用 `name = "..."` 覆盖),两者相互独立。
    fn name() -> &'static str {
        std::any::type_name::<Self>()
    }

    fn run(ctx: &Self::Ctx) -> impl Future<Output = std::result::Result<(), Self::Error>>;

    /// 预检,默认直接通过;按需覆盖。
    /// 返回 `Err` 表示预检本身出错,同样记为一次 validate 失败。
    fn validate(_ctx: &Self::Ctx) -> impl Future<Output = std::result::Result<bool, Self::Error>> {
        async move { Ok(true) }
    }
}
