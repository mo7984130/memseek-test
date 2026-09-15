use std::{borrow::Cow, fmt::Debug};

/// 场景错误统一接口:需实现 [`Debug`] 与错误分类 [`ScenarioError::kind`]。
///
/// 新增 [`ScenarioError::is_timeout`] 用于区分“软失败”:请求超时被视为
/// 被测系统过载信号,不计入 `failures`,而是单列为 `timeouts` 并触发退避。
pub trait ScenarioError: Debug {
    fn kind(&self) -> Cow<'static, str>;

    /// 是否属于超时类软失败(默认否)。
    /// 为 `true` 时:不计入失败,计入超时统计;runner 在下一轮前退避等待。
    fn is_timeout(&self) -> bool {
        false
    }
}
