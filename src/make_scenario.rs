//! `scenario!` 宏:定义并注册一个场景。
//!
//! 用法:
//! ```ignore
//! memseek_test::scenario! {
//!     HelloScenario,
//!     ctx = AuthContext,
//!     error = ctxlibs::http_client::HttpError,
//!     mode = memseek_test::RunMode::Times(10000),  // 可选:执行模式,不写则 mode 为空
//!     // 并发由 Manager 统一管理。
//!     run: ctx => {                // `ctx` 是参数名(macro_rules 卫生性:须由调用方提供)
//!         let resp = ctx.client.get("/hello").await?;
//!         Ok(())
//!     }
//! }
//! ```
//!
//! 宏生成:
//! 1. `pub struct HelloScenario`(unit struct,自动 `Default`);
//! 2. `impl Scenario`(run 自动包成 `Pin<Box<dyn Future + '_>>`,
//!    使用者不需要写 `Box::pin` / `async move`);
//! 3. 一个 `ScenarioRegistration` 条目(含场景配置),编译期 `inventory::submit!`。

/// 定义并注册一个场景。
///
/// 配置项:
/// - `mode = RunMode::Times(n)` 或 `mode = RunMode::Duration(d)`:执行模式(可选,
///   不写则 mode 为空,由 Manager 的 `with_run_mode` 覆盖/决定);
/// 并发由 Manager 统一管理(`ManagerConfig::new(concurrency)`)。
#[macro_export]
macro_rules! scenario {
    (
        $ty:ident,
        ctx = $ctx:ty,
        error = $err:ty,
        $(name = $name:literal,)?
        $(mode = $mode:expr,)?
        run: $run:ident => $body:block
    ) => {
        #[derive(Default)]
        pub struct $ty;

        impl $crate::scenario::Scenario for $ty {
            type Ctx = $ctx;
            type Error = $err;

            fn name() -> &'static str {
                $crate::__scenario_name!(
                    $ty
                    $(, $name)?
                )
            }

            fn run($run: &Self::Ctx)
                -> ::core::pin::Pin<
                    ::std::boxed::Box<
                        dyn ::core::future::Future<
                            Output = ::core::result::Result<(), Self::Error>,
                        > + '_,
                    >,
                >
            {
                // Box::pin + async move 在这里生成,使用者不写
                ::std::boxed::Box::pin(async move $body)
            }
        }

        $crate::register_scenario!(
            $ty,
            $crate::__scenario_opt!($($mode)?)
        );
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

/// 内部宏:#[doc(hidden)],被 `scenario!` 调用,生成 inventory 注册条目。
#[doc(hidden)]
#[macro_export]
macro_rules! register_scenario {
    ($ty:ident, $mode:expr) => {
        $crate::inventory::submit! {
            $crate::registry::ScenarioRegistration {
                name: ::core::stringify!($ty),
                ctx_type: || ::core::any::TypeId::of::<
                    <$ty as $crate::scenario::Scenario>::Ctx,
                >(),
                config: $crate::registry::ScenarioConfig::new(
                    $mode,
                ),
                invoke: $crate::registry::invoke::<$ty>,
            }
        }
    };
}
