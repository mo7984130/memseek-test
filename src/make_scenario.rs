//! 场景注册:`register_scenario!` 宏。
//!
//! 与旧 `scenario!` 宏不同,这里**业务代码外置**:使用者手写普通
//! `struct` + `impl Scenario`(真实代码,rust-analyzer 可完整分析,
//! 支持补全/跳转/诊断),`register_scenario!` 只负责生成 inventory 注册条目。
//!
//! 用法:
//! ```ignore
//! use memseek_test::{register_scenario, scenario::Scenario, RunMode};
//!
//! struct HelloScenario;
//!
//! impl Scenario for HelloScenario {
//!     type Ctx = AuthContext;
//!     type Error = ctxlibs::http_client::HttpError;
//!
//!     async fn run(ctx: &AuthContext) -> Result<(), Self::Error> {
//!         let resp = ctx.client.get("/hello").await?;
//!         Ok(())
//!     }
//!
//!     // validate() / name() 均有默认实现,按需覆盖即可
//! }
//!
//! // 在模块底部注册。可选配置:
//! //   name = "..." 覆盖匹配名(默认类型简单名)
//! //   mode = RunMode::Times(n) | RunMode::Duration(d) 设置默认执行模式
//! register_scenario!(HelloScenario, mode = RunMode::Times(10000));
//! ```
//!
//! 并发由 Manager 统一管理(`ManagerConfig::new(concurrency)`)。

/// 注册一个已实现 `Scenario` 的场景类型,生成 inventory 注册条目。
///
/// 可选参数:
/// - `name = "..."`:覆盖匹配名(默认类型简单名);
/// - `mode = RunMode::Times(n)` / `RunMode::Duration(d)`:默认执行模式
///   (缺省为 `None`,由 Manager 的 `with_run_mode` 覆盖/决定)。
#[macro_export]
macro_rules! register_scenario {
    (
        $ty:ident
        $(, name = $name:literal)?
        $(, mode = $mode:expr)?
    ) => {
        $crate::inventory::submit! {
            $crate::registry::ScenarioRegistration {
                name: $crate::__scenario_name!($ty $(, $name)?),
                ctx_type: || ::core::any::TypeId::of::<
                    <$ty as $crate::scenario::Scenario>::Ctx,
                >(),
                config: $crate::registry::ScenarioConfig::new(
                    $crate::__scenario_opt!($($mode)?)
                ),
                invoke: $crate::registry::invoke::<$ty>,
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scenario_name {
    ($ty:ident, $name:literal) => {
        $name
    };

    ($ty:ident) => {
        ::core::stringify!($ty)
    };
}

/// 可选参数 → `Option` 值(缺省为 `None`,给出则 `Some(expr)`)。
#[doc(hidden)]
#[macro_export]
macro_rules! __scenario_opt {
    () => {
        ::core::option::Option::None
    };
    ($e:expr) => {
        ::core::option::Option::Some($e)
    };
}
