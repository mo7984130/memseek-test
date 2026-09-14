//! 实时进度显示(Manager 可选功能)。
//!
//! 设计约束:
//!
//! - **只读旁路**:进度只累加共享原子计数,展示的轮数/失败数与最终报告字段
//!   一一对应(`times` / `failures` / `validate_failures`),但最终统计仍以
//!   merge 后的 [`Recorder`](crate::recorder::Recorder) 为准,进度永不影响结果;
//! - **热路径零锁**:并发任务每次记录只做 `Relaxed` 原子加,渲染在独立 Tokio
//!   任务中按 [`ProgressOptions::refresh`] 采样,不阻塞正在等响应的业务任务;
//! - **默认静默**:stderr 非 TTY(CI/重定向)时不输出,可用
//!   [`ProgressOptions::force`] 强制开启;
//! - 单行显示,覆盖当前场景。每个场景结束后该行定稿(补换行)保留在终端,
//!   下一个场景另起一行。若业务侧同时向 stderr 打印(如 `tracing` 输出错误),
//!   可能与进度行交错,下一次刷新会重绘修复。

use std::{
    borrow::Cow,
    fmt::Write as _,
    io::IsTerminal,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::task::JoinHandle;

use crate::{
    report::{bar, fmt_duration, paint},
    shutdown::Shutdown,
};

/// 进度显示选项。
#[derive(Debug, Clone, Copy)]
pub struct ProgressOptions {
    /// 强制开启:即使 stderr 不是 TTY 也输出。
    /// 默认 `false`,即非 TTY 环境(CI、重定向)自动静默。
    pub force: bool,
    /// 是否启用 ANSI 颜色,默认 `false`(与
    /// [`ReportOptions`](crate::report::ReportOptions) 一致,需显式开启)
    pub color: bool,
    /// 刷新间隔
    pub refresh: Duration,
    /// 进度条宽度(字符数)
    pub bar_width: usize,
}

impl Default for ProgressOptions {
    fn default() -> Self {
        Self {
            force: false,
            color: false,
            refresh: Duration::from_millis(100),
            bar_width: 20,
        }
    }
}

/// 进度计划的计量基准。
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
    options: ProgressOptions,
    started: Instant,
    /// 已完成轮数(与报告 `times` 对齐:由 `record_duration` 累加)
    rounds: AtomicU64,
    failures: AtomicU64,
    validate_failures: AtomicU64,
    /// 当前在途请求数(`begin_round` / `end_round` 维护)
    in_flight: AtomicU64,
    /// 当前处于 setup 阶段的任务数
    in_setup: AtomicU64,
    /// 渲染循环停止标志
    stop: AtomicBool,
}

impl Progress {
    pub(crate) fn new(
        name: impl Into<Cow<'static, str>>,
        plan: ProgressPlan,
        options: ProgressOptions,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                name: name.into(),
                plan,
                options,
                started: Instant::now(),
                rounds: AtomicU64::new(0),
                failures: AtomicU64::new(0),
                validate_failures: AtomicU64::new(0),
                in_flight: AtomicU64::new(0),
                in_setup: AtomicU64::new(0),
                stop: AtomicBool::new(false),
            }),
        }
    }

    /// 启动渲染任务(须在 Tokio runtime 内调用),返回停止守卫。
    /// stderr 非 TTY 且未 `force` 时返回不渲染的空守卫。
    pub(crate) fn start(&self, shutdown: Option<Shutdown>) -> ProgressGuard {
        if !self.inner.options.force && !std::io::stderr().is_terminal() {
            return ProgressGuard {
                inner: Arc::clone(&self.inner),
                handle: None,
            };
        }
        self.spawn(std::io::stderr(), shutdown)
    }

    /// 渲染循环(出口可替换,便于测试断言输出内容)。
    fn spawn<W>(&self, writer: W, shutdown: Option<Shutdown>) -> ProgressGuard
    where
        W: std::io::Write + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let task_inner = Arc::clone(&inner);
        let handle = tokio::spawn(async move {
            let mut writer = writer;
            loop {
                let stopping = shutdown.as_ref().is_some_and(|s| s.is_cancelled());
                let line = render_line(&task_inner, stopping);
                // 单次写入 + 清行,减少与其它 stderr 输出的交错窗口
                let _ = write!(writer, "\r{line}\x1b[K");
                let _ = writer.flush();

                if task_inner.stop.load(Ordering::Relaxed) {
                    // 定稿:补换行,保留最后一行
                    let _ = writeln!(writer);
                    let _ = writer.flush();
                    break;
                }
                tokio::time::sleep(task_inner.options.refresh).await;
            }
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
        if !ok {
            self.inner.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 记录一轮 validate 结果
    pub(crate) fn validate_recorded(&self, ok: bool) {
        if !ok {
            self.inner.validate_failures.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 渲染任务的停止守卫:drop 即停止(取消防抖路径),正常路径应调用
/// [`ProgressGuard::stop`] 以等待最后一行定稿。
pub(crate) struct ProgressGuard {
    inner: Arc<Inner>,
    handle: Option<JoinHandle<()>>,
}

impl ProgressGuard {
    /// 停止渲染并等待最后一行写入完成。
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

/// 组装单行进度文本(纯函数,便于测试)。
fn render_line(inner: &Inner, stopping: bool) -> String {
    let name = fit_name(&inner.name);
    let color = inner.options.color;
    let mut line = String::with_capacity(128);

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
        bar(fraction, inner.options.bar_width),
        fraction * 100.0,
        rate(rounds, elapsed),
    );

    let failures =
        inner.failures.load(Ordering::Relaxed) + inner.validate_failures.load(Ordering::Relaxed);
    if failures > 0 {
        let _ = write!(
            line,
            "  {}",
            paint(&format!("fail {failures}"), "31", color)
        );
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
    use std::sync::Mutex;

    use super::*;

    /// 可跨任务共享的输出缓冲
    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl SharedBuf {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn line_shows_rounds_percent_and_rate() {
        let p = Progress::new(
            "login",
            ProgressPlan::Rounds(1000),
            ProgressOptions::default(),
        );
        for _ in 0..250 {
            p.round_recorded();
        }
        p.result_recorded(false);

        let line = render_line(&p.inner, false);
        assert!(line.contains("login"), "{line}");
        assert!(line.contains("25.0%"), "{line}");
        assert!(line.contains("250/1000"), "{line}");
        assert!(line.contains("req/s"), "{line}");
        assert!(line.contains("fail 1"), "{line}");
        // 默认无颜色
        assert!(!line.contains('\x1b'), "{line}");
        // 未进入 setup、无在途请求时不显示这些片段
        assert!(!line.contains("setup"), "{line}");
        assert!(!line.contains("inflight"), "{line}");
    }

    #[test]
    fn line_reports_setup_phase_and_stopping() {
        let p = Progress::new("scn", ProgressPlan::Rounds(10), ProgressOptions::default());
        p.begin_setup();
        let line = render_line(&p.inner, false);
        assert!(line.contains("setup..."), "{line}");
        assert!(!line.contains("req/s"), "{line}");

        p.end_setup();
        p.begin_round();
        let line = render_line(&p.inner, true);
        assert!(line.contains("inflight 1"), "{line}");
        assert!(line.contains("stopping..."), "{line}");
    }

    #[test]
    fn duration_plan_uses_elapsed_over_planned() {
        let p = Progress::new(
            "dur",
            ProgressPlan::Time(Duration::from_secs(600)),
            ProgressOptions::default(),
        );
        let line = render_line(&p.inner, false);
        assert!(line.contains("0.0%"), "{line}");
        assert!(line.contains("/600.00s"), "{line}");
    }

    #[test]
    fn long_name_is_truncated_to_fixed_width() {
        let p = Progress::new(
            "a_very_long_scenario_name_indeed",
            ProgressPlan::Rounds(1),
            ProgressOptions::default(),
        );
        let line = render_line(&p.inner, false);
        assert!(line.starts_with("a_very_long_scenari…"), "{line}");
    }

    #[test]
    fn color_option_paints_failures() {
        let p = Progress::new(
            "scn",
            ProgressPlan::Rounds(1),
            ProgressOptions {
                color: true,
                ..ProgressOptions::default()
            },
        );
        p.result_recorded(false);
        let line = render_line(&p.inner, false);
        assert!(line.contains("\x1b[31mfail 1\x1b[0m"), "{line}");
    }

    #[test]
    fn refresh_loop_renders_and_finalizes_line() {
        let buf = SharedBuf::default();
        let p = Progress::new(
            "scn",
            ProgressPlan::Rounds(4),
            ProgressOptions {
                refresh: Duration::from_millis(5),
                ..ProgressOptions::default()
            },
        );
        runtime().block_on(async {
            let guard = p.spawn(buf.clone(), None);
            for _ in 0..4 {
                p.round_recorded();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            guard.stop().await;
        });

        let text = buf.text();
        assert!(text.contains("scn"), "{text}");
        assert!(text.contains("100.0%"), "{text}");
        // 每次刷新回到行首清行,结束时补换行定稿
        assert!(text.starts_with('\r'), "{text:?}");
        assert!(text.ends_with("\x1b[K\n"), "{text:?}");
    }

    /// 非 TTY 且未 force 时 `start` 不启动渲染任务(stderr 已是 TTY 时跳过)。
    #[test]
    fn start_is_silent_when_not_a_tty() {
        if std::io::stderr().is_terminal() {
            return;
        }
        let p = Progress::new("scn", ProgressPlan::Rounds(4), ProgressOptions::default());
        runtime().block_on(async {
            let guard = p.start(None);
            assert!(guard.handle.is_none(), "未 force 的非 TTY 环境不应渲染");
            guard.stop().await;
        });
    }
}
