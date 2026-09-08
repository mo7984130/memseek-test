use crate::{error::ScenarioError, runner::TaskIndex};

/// 框架按单线程 `block_on` + `join_all` 调度,不 `tokio::spawn`,
/// 因此不要求方法返回的 Future `Send`;若未来需要 spawn,
/// 应改为返回 `impl Future + Send` 的手写形态。
#[allow(async_fn_in_trait)]
pub trait Scenario: Default + Send + Sync + 'static {
    type Ctx: Send + Sync;
    type Error: ScenarioError;
    /// `run` 的成功产出(如 `reqwest::Response`),每轮 run 的局部值,
    /// 仅供本轮 `validate` 借用来做业务校验。
    /// 不需要产出数据时填 `()` 即可(此时 `run` 签名与旧版 `Result<(), Error>` 等价)。
    type Output;

    /// 场景展示名。
    ///
    /// 默认返回完整类型路径(如 `my_mod::HelloScenario`)。
    /// 注意:注册进 registry 的**匹配名**由 `register_scenario!` 生成,
    /// 默认是类型简单名(可用 `name = "..."` 覆盖),两者相互独立。
    fn name() -> &'static str {
        std::any::type_name::<Self>()
    }

    /// 执行一次场景业务,返回本轮产出供 `validate` 校验。
    /// `task` 是框架分配的并发任务身份(编号/总数),可用 `task.index`
    /// 做账号等参数化(如 `format!(\"loadtest_{}\", task.index + 1)`)。
    /// 返回 `Err` 时该轮直接记为失败,runner 不会调用 `validate`。
    async fn run(ctx: &Self::Ctx, task: &TaskIndex) -> Result<Self::Output, Self::Error>;

    /// 对 run 的成功产出做业务校验,默认直接通过。
    /// `task` 与 `run` 收到的为同一任务身份。
    /// 返回 `Err` 同样记为一次 validate 失败。
    async fn validate(
        _ctx: &Self::Ctx,
        _task: &TaskIndex,
        _output: &Self::Output,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}
