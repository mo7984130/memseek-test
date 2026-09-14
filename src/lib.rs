pub mod make_scenario;
pub mod manager;
#[cfg(feature = "tui")]
pub mod progress;
pub mod registry;
pub mod report;
pub mod runner;
pub mod scenario;
pub mod shutdown;
#[cfg(feature = "tui")]
pub mod tui;

pub mod error;

pub mod ctxlibs;
pub mod recorder;

#[cfg(feature = "tui")]
pub use progress::{LogChannel, Progress};
pub use report::{Report, ReportOptions, ScenarioReport};
pub use runner::{RunMode, TaskIndex};
pub use shutdown::{Shutdown, ShutdownSender};

// 让 #[macro_export] 宏展开时能写 $crate::inventory::submit!,
// 使用者的 Cargo.toml 不需要显式依赖 inventory
pub use inventory;
