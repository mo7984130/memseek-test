#![cfg(feature = "tui")]

//! 实时进度计数与 TUI 渲染支撑(进度渲染见 [`crate::tui`],feature `tui`)。
//!
//! 设计约束:
//!
//! - **只读旁路**:进度只累加共享原子计数,展示的轮数/失败数与最终报告字段
//!   一一对应(`times` / `failures` / `validate_failures`),但最终统计仍以
//!   merge 后的 [`Recorder`](crate::recorder::Recorder) 为准,进度永不影响结果;
//! - **热路径零锁**:并发任务每次记录只做 `Relaxed` 原子加,渲染在独立 Tokio
//!   任务中按刷新间隔采样,不阻塞正在等响应的业务任务;
//! - **仅 TUI 形态**:进度只随 feature `tui` 的 `with_tui` 启用,
//!   非 TTY(CI/重定向)自动静默;
//! - 本模块还提供 [`LogChannel`] 日志环形缓冲:框架内部警告与用户 tracing
//!   日志都经它进日志区,避免与进度行字节级交错。

use std::{
    borrow::Cow,
    collections::VecDeque,
    fmt::Write as _,
    io::IsTerminal,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::task::JoinHandle;

use crate::{
    report::{bar, fmt_duration, paint},
    shutdown::Shutdown,
};

/// 日志环形缓冲(TUI 模式的日志区数据源)。
///
/// 实现 [`std::io::Write`](https://doc.rust-lang.org/std/io/trait.Write.html):
/// 按行切分入队,超容量丢弃最旧;`Clone` 共享同一缓冲。
/// 配合 feature `tui` 的 [`crate::tui::TuiOptions`] 使用:
/// 也可作为 `tracing_subscriber::fmt::layer().with_writer(channel)`
/// 的 writer,把业务日志接入日志区。
#[derive(Clone, Debug)]
pub struct LogChannel {
    inner: Arc<Mutex<LogBuf>>,
}

#[derive(Debug)]
struct LogBuf {
    lines: VecDeque<String>,
    max_lines: usize,
}

impl Default for LogChannel {
    fn default() -> Self {
        Self::with_capacity(200)
    }
}

impl LogChannel {
    /// 创建环形缓冲,最多保留 `max_lines` 行。
    pub fn with_capacity(max_lines: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogBuf {
                lines: VecDeque::with_capacity(max_lines.min(1024)),
                max_lines,
            })),
        }
    }

    /// 推送一行(内部会去掉行尾换行)。
    pub fn push_line(&self, line: impl Into<String>) {
        let mut buf = self.inner.lock().unwrap();
        let line = line.into();
        let line = line.strip_suffix('\n').unwrap_or(&line).to_string();
        if buf.lines.len() == buf.max_lines {
            buf.lines.pop_front();
        }
        buf.lines.push_back(line);
    }

    /// 当前缓冲内容(旧→新)。
    pub fn lines(&self) -> Vec<String> {
        self.inner.lock().unwrap().lines.iter().cloned().collect()
    }

    /// 取出全部内容并清空(用于 TUI 退出后的日志回放)。
    pub fn drain(&self) -> Vec<String> {
        let mut buf = self.inner.lock().unwrap();
        buf.lines.drain(..).collect()
    }
}

impl std::io::Write for LogChannel {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        for line in text.split('\n') {
            if !line.is_empty() {
                self.push_line(line);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 进度计划(Manager 解析 `RunMode` 后注入)。
#[derive(Debug, Clone, Copy)]
pub(crate) enum ProgressPlan {
    /// 总轮数(`RunMode::Times`)
    Rounds(u64),
    /// 总时长(`RunMode::Duration`)
    Time(Duration),
}

/// 实时进度句柄,克隆共享同一份计数。
///
/// 由 [`ScenarioManager`](crate::manager::ScenarioManager) 按场景创建并注入
/// [`RunnerConfig`](crate::runner::RunnerConfig);手动构造 `RunnerConfig`
/// 时填 `None` 即可,仅不显示进度,功能不受影响。
#[derive(Clone, Debug)]
pub struct Progress {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    name: Cow<'static, str>,
    plan: ProgressPlan,
    /// 当前场景序号(0 起)与场景总数;`total <= 1` 时不显示 `[n/N]`
    scenario_index: usize,
    scenario_total: usize,
    started: Instant,
    /// 已完成轮数(与报告 `times` 对齐:由 `record_duration` 累加)
    rounds: AtomicU64,
    /// run 成功次数(与报告 `success` 对齐)
    success: AtomicU64,
    failures: AtomicU64,
    /// 超时类软失败次数(与报告 `timeouts` 对齐,由 `timeout_recorded` 累加)
    timeouts: AtomicU64,
    validate_failures: AtomicU64,
    /// 当前在途请求数(`begin_round` / `end_round` 维护)
    in_flight: AtomicU64,
    /// 当前处于 setup 阶段的任务数
    in_setup: AtomicU64,
    /// 渲染循环停止标志
    stop: AtomicBool,
    /// TUI 模式激活时持有日志缓冲(否则为 `None`,不采集)
    log: Option<LogChannel>,
}

impl Progress {
    pub(crate) fn new(
        name: impl Into<Cow<'static, str>>,
        plan: ProgressPlan,
        log: Option<LogChannel>,
        scenario_index: usize,
        scenario_total: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                name: name.into(),
                plan,
                scenario_index,
                scenario_total,
                started: Instant::now(),
                rounds: AtomicU64::new(0),
                success: AtomicU64::new(0),
                failures: AtomicU64::new(0),
                timeouts: AtomicU64::new(0),
                validate_failures: AtomicU64::new(0),
                in_flight: AtomicU64::new(0),
                in_setup: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                log,
            }),
        }
    }
    // ---------- 热路径钩子(由 Recorder / ScenarioRunner 调用) ----------

    /// TUI 模式渲染任务(feature `tui`,须在 Tokio runtime 内调用)。
    /// 进入 alternate screen 全屏渲染:顶部进度行 + 日志区;
    /// 停止时恢复原屏幕。非 TTY 时返回不渲染的空守卫。
    #[cfg(feature = "tui")]
    pub(crate) fn start_tui(
        &self,
        channel: crate::LogChannel,
        options: crate::tui::TuiOptions,
        shutdown: Option<Shutdown>,
    ) -> ProgressGuard {
        if !std::io::stderr().is_terminal() {
            return ProgressGuard {
                inner: Arc::clone(&self.inner),
                handle: None,
            };
        }
        let inner = Arc::clone(&self.inner);
        let task_inner = Arc::clone(&inner);
        let handle = tokio::spawn(async move {
            let mut writer = std::io::stderr();
            // 进入 alternate screen
            let _ = std::io::Write::write(&mut writer, b"\x1b[?1049h");
            let _ = std::io::Write::flush(&mut writer);
            loop {
                let stopping = shutdown.as_ref().is_some_and(|s| s.is_cancelled());
                let line = render_line(&task_inner, stopping, options.color, options.bar_width);
                let frame = crate::tui::render_frame(&line, &channel, &options);
                // 回到左上角后清屏重绘,避免残留
                let text = format!("\x1b[H\x1b[2J{frame}");
                let _ = std::io::Write::write(&mut writer, text.as_bytes());
                let _ = std::io::Write::flush(&mut writer);

                if task_inner.stop.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(options.refresh).await;
            }
            // 恢复原屏幕
            let _ = std::io::Write::write(&mut writer, b"\x1b[?1049l");
            let _ = std::io::Write::flush(&mut writer);
        });
        ProgressGuard {
            inner,
            handle: Some(handle),
        }
    }

    // ---------- 热路径钩子(由 Recorder / ScenarioRunner 调用) ----------

    /// 一轮 `run` 开始:在途 +1
    pub(crate) fn begin_round(&self) {
        self.inner.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    /// 一轮 `run` 结束:在途 -1
    pub(crate) fn end_round(&self) {
        self.inner.in_flight.fetch_sub(1, Ordering::Relaxed);
    }

    /// 进入 setup 阶段
    pub(crate) fn begin_setup(&self) {
        self.inner.in_setup.fetch_add(1, Ordering::Relaxed);
    }

    /// 退出 setup 阶段
    pub(crate) fn end_setup(&self) {
        self.inner.in_setup.fetch_sub(1, Ordering::Relaxed);
    }

    /// 记录一轮耗时(与报告 `times` 同步累加)
    pub(crate) fn round_recorded(&self) {
        self.inner.rounds.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一轮 run 结果
    pub(crate) fn result_recorded(&self, ok: bool) {
        if ok {
            self.inner.success.fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 记录一轮超时软失败(不计入 failures)
    pub(crate) fn timeout_recorded(&self) {
        self.inner.timeouts.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一轮 validate 结果
    pub(crate) fn validate_recorded(&self, ok: bool) {
        if !ok {
            self.inner.validate_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 框架内部日志进 TUI 日志缓冲(带时间前缀);无缓冲时返回 `false`,由调用方落 tracing。
    pub(crate) fn push_log(&self, line: &str) -> bool {
        if let Some(channel) = &self.inner.log {
            channel.push_line(format!("[{} {line}]", now_hms()));
            true
        } else {
            false
        }
    }
}

/// 渲染任务的停止守卫:drop 即停止(异常路径兜底),正常路径应调用
/// [`ProgressGuard::stop`] 以等待渲染任务收尾(恢复屏幕)。
pub(crate) struct ProgressGuard {
    inner: Arc<Inner>,
    handle: Option<JoinHandle<()>>,
}

impl ProgressGuard {
    /// 停止渲染并等待任务收尾。
    pub(crate) async fn stop(mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

// ---------- 渲染 ----------

/// 场景名展示宽度(按字符截断,保证单行长度可控)
const NAME_WIDTH: usize = 20;

/// 组装进度行文本(纯函数,便于测试);供 TUI 帧渲染使用。
/// 多场景时带 `[n/N]` 前缀(单场景不显示)。
fn render_line(inner: &Inner, stopping: bool, color: bool, bar_width: usize) -> String {
    let name = fit_name(&inner.name);
    let mut line = String::with_capacity(128);
    if inner.scenario_total > 1 {
        let _ = write!(
            line,
            "[{}/{}] ",
            inner.scenario_index + 1,
            inner.scenario_total
        );
    }

    // setup 阶段(仅 Task 粒度):轮数尚未开始
    if inner.in_setup.load(Ordering::Relaxed) > 0 {
        let _ = write!(line, "{name} {}", paint("setup...", "33", color));
        return line;
    }

    let elapsed = inner.started.elapsed();
    let rounds = inner.rounds.load(Ordering::Relaxed);
    let (fraction, progress) = match inner.plan {
        ProgressPlan::Rounds(total) => (ratio(rounds, total), format!("{rounds}/{total}")),
        ProgressPlan::Time(planned) => (
            ratio(elapsed.as_millis() as u64, planned.as_millis() as u64),
            format!("{}/{}", fmt_duration(elapsed), fmt_duration(planned)),
        ),
    };

    let _ = write!(
        line,
        "{name} [{}] {:>5.1}%  {progress:>15}  {:>7} req/s",
        bar(fraction, bar_width),
        fraction * 100.0,
        rate(rounds, elapsed),
    );

    // 成功/失败对账:succ = 有效通过(run 成功且 validate 通过),
    // err = run 失败 + validate 失败;超时软失败单独以黄色显示
    if rounds > 0 {
        let passed = inner
            .success
            .load(Ordering::Relaxed)
            .saturating_sub(inner.validate_failures.load(Ordering::Relaxed));
        let err = inner.failures.load(Ordering::Relaxed)
            + inner.validate_failures.load(Ordering::Relaxed);
        let timeouts = inner.timeouts.load(Ordering::Relaxed);
        let _ = write!(line, "  succ {passed}");
        if err > 0 {
            let _ = write!(line, "  {}", paint(&format!("err {err}"), "31", color));
        }
        if timeouts > 0 {
            let _ = write!(
                line,
                "  {}",
                paint(&format!("timeout {timeouts}"), "33", color)
            );
        }
    }

    let in_flight = inner.in_flight.load(Ordering::Relaxed);
    if in_flight > 0 {
        let _ = write!(line, "  inflight {in_flight}");
    }

    if stopping {
        let _ = write!(line, "  {}", paint("stopping...", "33", color));
    }

    line
}

/// 名称按字符截断到 [`NAME_WIDTH`],不足补空格对齐
fn fit_name(name: &str) -> String {
    if name.chars().count() <= NAME_WIDTH {
        format!("{name:<NAME_WIDTH$}")
    } else {
        let head: String = name.chars().take(NAME_WIDTH - 1).collect();
        format!("{head}…")
    }
}

/// `done / total` → 0.0 ~ 1.0;`total` 为 0 视为已完成
fn ratio(done: u64, total: u64) -> f64 {
    if total == 0 {
        return 1.0;
    }
    (done as f64 / total as f64).clamp(0.0, 1.0)
}

/// 当前本地时间 `HH:MM:SS`(用于框架日志的时间前缀)
fn now_hms() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (h, m, s) = (now / 3600 % 24, now / 60 % 60, now % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

/// 累计平均吞吐(attempts / 已用时),计 0 或耗时过短时返回 0
fn rate(rounds: u64, elapsed: Duration) -> u64 {
    let secs = elapsed.as_secs_f64();
    if secs < 0.001 {
        return 0;
    }
    (rounds as f64 / secs) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_shows_rounds_percent_and_rate() {
        let p = Progress::new("login", ProgressPlan::Rounds(1000), None, 1, 3);
        for _ in 0..250 {
            p.round_recorded();
        }
        for _ in 0..249 {
            p.result_recorded(true);
        }
        p.result_recorded(false);
        for _ in 0..249 {
            p.validate_recorded(true);
        }
        for _ in 0..3 {
            p.timeout_recorded();
        }

        let line = render_line(&p.inner, false, false, 20);
        assert!(line.contains("login"), "{line}");
        assert!(line.contains("[2/3]"), "{line}");
        assert!(line.contains("25.0%"), "{line}");
        assert!(line.contains("250/1000"), "{line}");
        assert!(line.contains("req/s"), "{line}");
        // 成功/失败对账:succ + err = 已完成轮数
        assert!(line.contains("succ 249"), "{line}");
        assert!(line.contains("err 1"), "{line}");
        // 超时软失败单独计数,黄字着色由 color_option 覆盖
        assert!(line.contains("timeout 3"), "{line}");
        // fail 已由 err 取代,不得残留
        assert!(!line.contains("fail"), "{line}");
        // 默认无颜色
        assert!(!line.contains('\x1b'), "{line}");
        // 未进入 setup、无在途请求时不显示这些片段
        assert!(!line.contains("setup"), "{line}");
        assert!(!line.contains("inflight"), "{line}");
    }

    #[test]
    fn line_reports_setup_phase_and_stopping() {
        let p = Progress::new("scn", ProgressPlan::Rounds(10), None, 0, 2);
        p.begin_setup();
        let line = render_line(&p.inner, false, false, 20);
        assert!(line.contains("setup..."), "{line}");
        assert!(line.contains("[1/2]"), "{line}");
        assert!(!line.contains("req/s"), "{line}");

        p.end_setup();
        p.begin_round();
        let line = render_line(&p.inner, true, false, 20);
        assert!(line.contains("inflight 1"), "{line}");
        assert!(line.contains("stopping..."), "{line}");
        // 无轮次完成时不显示 succ/err
        assert!(!line.contains("succ"), "{line}");
        assert!(!line.contains("err"), "{line}");
    }

    #[test]
    fn duration_plan_uses_elapsed_over_planned() {
        let p = Progress::new(
            "dur",
            ProgressPlan::Time(Duration::from_secs(600)),
            None,
            0,
            1,
        );
        let line = render_line(&p.inner, false, false, 20);
        assert!(line.contains("0.0%"), "{line}");
        assert!(line.contains("/600000.00ms"), "{line}");
    }

    #[test]
    fn long_name_is_truncated_to_fixed_width() {
        let p = Progress::new(
            "a_very_long_scenario_name_indeed",
            ProgressPlan::Rounds(1),
            None,
            0,
            3,
        );
        let line = render_line(&p.inner, false, false, 20);
        assert!(line.starts_with("[1/3] a_very_long_scenari…"), "{line}");
    }

    #[test]
    fn single_scenario_hides_index_badge() {
        let p = Progress::new("only", ProgressPlan::Rounds(1), None, 0, 1);
        let line = render_line(&p.inner, false, false, 20);
        assert!(!line.contains("[1/1]"), "单场景不应显示序号: {line}");
        assert!(line.starts_with("only"), "{line}");
    }

    #[test]
    fn color_option_paints_failures() {
        let p = Progress::new("scn", ProgressPlan::Rounds(1), None, 0, 1);
        p.round_recorded();
        p.result_recorded(false);
        let line = render_line(&p.inner, false, true, 20);
        assert!(line.contains("\x1b[31merr 1\x1b[0m"), "{line}");
    }

    #[test]
    fn log_channel_ring_buffer_and_write() {
        let mut channel = LogChannel::with_capacity(3);
        channel.push_line("a");
        channel.push_line("b");
        channel.push_line("c");
        channel.push_line("d"); // 超容量丢最旧
        assert_eq!(channel.lines(), vec!["b", "c", "d"]);

        // io::Write 按行切分,行尾换行不残留
        std::io::Write::write_all(&mut channel, b"e\nf\n").unwrap();
        assert_eq!(channel.lines(), vec!["d", "e", "f"]);

        // drain 取出并清空
        assert_eq!(channel.drain(), vec!["d", "e", "f"]);
        assert!(channel.lines().is_empty());
    }

    #[test]
    fn push_log_prefixes_time_in_tui_mode() {
        let channel = LogChannel::with_capacity(10);
        let p = Progress::new("s", ProgressPlan::Rounds(1), Some(channel.clone()), 0, 1);
        assert!(p.push_log("boom"));
        let lines = channel.lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with('['), "{lines:?}");
        assert!(lines[0].ends_with("boom]"), "{lines:?}");

        // 无日志通道时返回 false,调用方走 tracing 兜底
        let p = Progress::new("s", ProgressPlan::Rounds(1), None, 0, 1);
        assert!(!p.push_log("x"));
    }
}
