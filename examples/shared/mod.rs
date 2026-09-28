//! 示例共用的小工具。
//!
//! `examples/` 下的子目录只要没有 `main.rs`,就不会被 cargo 当成独立示例,
//! 因此可以在这里放公共代码。

use std::borrow::Cow;

use memseek_test::error::ScenarioError;

/// 示例用的最小场景错误:任何实现 `Debug` 的类型,补一个 `kind()` 就是场景错误。
/// `kind()` 的分类会原样出现在报告的 `error_map` 里。
#[derive(Debug)]
pub struct DemoError(pub &'static str);

impl ScenarioError for DemoError {
    fn kind(&self) -> Cow<'static, str> {
        Cow::Borrowed(self.0)
    }
}
