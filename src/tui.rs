//! 终端 TUI 模式(需开启 feature `tui`)。
//!
//! 进入 alternate screen(`\x1b[?1049h`)全屏显示:顶部固定当前场景进度行,
//! 下方日志滚动区。所有终端字节都经渲染任务统一绘制:框架内部日志与业务
//! 日志先进 [`LogChannel`] 环形缓冲再入屏,消除多写入者的字节级交错。
//!
//! 业务日志接入(可选):把 [`LogChannel`] 作为 writer 挂到 tracing 上,
//! 日志区即可显示业务侧输出,且不替换用户已有的 subscriber:
//!
//! ```ignore
//! use tracing_subscriber::prelude::*;
//! use memseek_test::{
//!     LogChannel, manager::{ManagerConfig, ScenarioManager},
//! };
//!
//! let channel = LogChannel::with_capacity(500);
//! tracing_subscriber::registry()
//!     .with(tracing_subscriber::fmt::layer().with_writer(channel.clone()))
//!     .init();
//!
//! let manager = ScenarioManager::new(
//!     ManagerConfig::new(8).with_tui_options(memseek_test::tui::TuiOptions::default().with_channel(channel)),
//! );
//! ```
//!
//! 非 TTY(CI/重定向)时自动静默(不进入全屏);每次运行结束后日志缓冲
//! 回放到 stderr,保证日志档案完整。TUI 与 [`ManagerConfig::with_progress`]
//! 单行模式互斥。
//!
//! 已知限制:终端高度未知,日志区固定显示 [`TuiOptions::log_lines`] 行
//! (默认 12),若终端行数 < 进度区 + 日志区,进度行会被顶出可视区,
//! 请调小 `log_lines`。

use std::fmt::Write as _;
use std::time::Duration;

use crate::LogChannel;

/// 终端 TUI 显示选项。
#[derive(Debug, Clone)]
pub struct TuiOptions {
    /// 日志区固定显示行数(默认 12)
    pub log_lines: usize,
    /// 日志缓冲容量:超过丢最旧;TUI 退出时完整回放到 stderr(默认 500)
    pub buffer_lines: usize,
    /// 刷新间隔(默认 100ms)
    pub refresh: Duration,
    /// 进度条宽度(字符数,默认 20)
    pub bar_width: usize,
    /// 日志通道;`None` 时由 Manager 自动创建(仅收编框架内部日志)
    channel: Option<LogChannel>,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            log_lines: 12,
            buffer_lines: 500,
            refresh: Duration::from_millis(100),
            bar_width: 20,
            channel: None,
        }
    }
}

impl TuiOptions {
    /// 指定日志通道:同时作为用户 tracing 日志的接入点(见模块文档示例)。
    pub fn with_channel(mut self, channel: LogChannel) -> Self {
        self.channel = Some(channel);
        self
    }

    pub(crate) fn take_channel(&mut self) -> LogChannel {
        self.channel
            .take()
            .unwrap_or_else(|| LogChannel::with_capacity(self.buffer_lines))
    }
}

/// 组装一帧 TUI 画面(纯函数,便于测试):进度行 + 分隔线 + 最近日志。
pub(crate) fn render_frame(
    progress_line: &str,
    channel: &LogChannel,
    options: &TuiOptions,
) -> String {
    let mut frame = String::new();
    let _ = write!(frame, "{progress_line}\r\n");
    let _ = write!(frame, "{}\r\n", "─".repeat(options.bar_width + 32));

    let lines: Vec<String> = channel
        .lines()
        .into_iter()
        .rev()
        .take(options.log_lines)
        .rev()
        .collect();
    for line in lines {
        let line: String = line.chars().take(240).collect();
        let _ = write!(frame, "{line}\r\n");
    }

    frame
}

/// [`LogChannel`] 作为 `tracing_subscriber::fmt` writer 的适配:
/// 业务日志按行进入环形缓冲(供 TUI 日志区与退出回放使用)。
impl tracing_subscriber::fmt::MakeWriter<'_> for LogChannel {
    type Writer = LogChannel;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_fixture() -> LogChannel {
        let channel = LogChannel::with_capacity(20);
        for i in 0..5 {
            channel.push_line(format!("log line {i}"));
        }
        channel
    }

    #[test]
    fn frame_lays_out_progress_and_recent_logs() {
        let channel = frame_fixture();
        let frame = render_frame(
            "[████████░░░░] 50.0% 5/10",
            &channel,
            &TuiOptions::default(),
        );

        assert!(
            frame.starts_with("[████████░░░░] 50.0% 5/10\r\n"),
            "{frame:?}"
        );
        assert!(frame.contains("─"), "{frame:?}");
        assert!(frame.contains("log line 0\r\n"), "{frame:?}");
        assert!(frame.ends_with("log line 4\r\n"), "{frame:?}");
    }

    #[test]
    fn frame_keeps_only_recent_lines() {
        let channel = frame_fixture();
        let options = TuiOptions {
            log_lines: 2,
            ..TuiOptions::default()
        };
        let frame = render_frame("progress", &channel, &options);

        assert!(!frame.contains("log line 0"), "{frame:?}");
        assert!(!frame.contains("log line 2"), "{frame:?}");
        assert!(frame.ends_with("log line 4\r\n"), "{frame:?}");
    }

    #[test]
    fn frame_truncates_overlong_log_lines() {
        let channel = LogChannel::with_capacity(5);
        channel.push_line("x".repeat(300));
        let frame = render_frame("p", &channel, &TuiOptions::default());

        assert!(frame.contains(&"x".repeat(240)), "应截断到 240 字符");
        assert!(!frame.contains(&"x".repeat(241)), "{frame:?}");
    }

    #[test]
    fn make_writer_routes_fmt_lines_into_buffer() {
        use tracing_subscriber::fmt::MakeWriter as _;

        let channel = LogChannel::with_capacity(10);
        {
            let mut writer = channel.clone().make_writer();
            std::io::Write::write_all(&mut writer, b"hello from fmt\n").unwrap();
        }
        assert_eq!(channel.lines(), vec!["hello from fmt"]);
    }
}
