use std::{borrow::Cow, collections::HashMap, time::Duration};

use hdrhistogram::Histogram;
use tracing::warn;

#[cfg(feature = "tui")]
use crate::progress::Progress;
use crate::{error::ScenarioError, runner::RunnerConfig};

pub struct Recorder {
    histogram: Histogram<u64>,
    total: Duration,

    /// 实时进度上报句柄(feature `tui`,由 [`Recorder::from_config`] 注入);
    /// 所有进度钩子汇集在此:报告统计与进度展示的数字同源。
    #[cfg(feature = "tui")]
    progress: Option<Progress>,

    pub success: u64,
    pub failures: u64,
    /// 超时类软失败数(不加剧 `failures`;由 [`ScenarioError::is_timeout`] 判定)
    pub timeouts: u64,
    pub error_map: HashMap<Cow<'static, str>, u64>,

    pub validate_success: u64,
    pub validate_failures: u64,
    /// 各任务 `setup` 尝试耗时之和(含超时重试的每次尝试;与墙钟口径不同)
    pub setup_total: Duration,
    /// 各轮 `validate` 耗时之和
    pub validate_total: Duration,
    /// 各次 `teardown` 耗时之和
    pub teardown_total: Duration,
    /// 收尾成功/失败次数(失败不加剧 `failures`,单列统计)
    pub teardown_success: u64,
    pub teardown_failures: u64,
    /// 该任务是否因优雅关闭(停止信号)提前结束
    pub interrupted: bool,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            histogram: Histogram::new(3).unwrap(),
            total: Duration::ZERO,
            #[cfg(feature = "tui")]
            progress: None,
            success: 0,
            failures: 0,
            timeouts: 0,
            error_map: HashMap::new(),
            validate_success: 0,
            validate_failures: 0,
            setup_total: Duration::ZERO,
            validate_total: Duration::ZERO,
            teardown_total: Duration::ZERO,
            teardown_success: 0,
            teardown_failures: 0,
            interrupted: false,
        }
    }

    /// 从运行配置创建:`RunnerConfig::progress` 存在时,后续 `record_*`
    /// 会同步累加实时进度计数。
    pub fn from_config(_config: &RunnerConfig) -> Self {
        Self {
            #[cfg(feature = "tui")]
            progress: _config.progress.clone(),
            ..Self::new()
        }
    }

    pub fn total(&self) -> Duration {
        self.total
    }

    pub fn times(&self) -> u64 {
        self.histogram.len()
    }

    pub fn avg(&self) -> Duration {
        if self.histogram.is_empty() {
            return Duration::ZERO;
        }

        Duration::from_micros(self.histogram.mean() as u64)
    }

    pub fn min(&self) -> Duration {
        if self.histogram.is_empty() {
            Duration::ZERO
        } else {
            Duration::from_micros(self.histogram.min())
        }
    }

    pub fn max(&self) -> Duration {
        if self.histogram.is_empty() {
            Duration::ZERO
        } else {
            Duration::from_micros(self.histogram.max())
        }
    }

    pub fn p50(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.50))
    }

    pub fn p95(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.95))
    }

    pub fn p99(&mut self) -> Duration {
        Duration::from_micros(self.histogram.value_at_quantile(0.99))
    }

    pub fn merge(&mut self, other: Self) {
        self.histogram
            .add(other.histogram)
            .expect("merge Recorder failed");
        self.total += other.total;
        self.success += other.success;
        self.failures += other.failures;
        self.timeouts += other.timeouts;
        for (kind, count) in other.error_map {
            *self.error_map.entry(kind).or_insert(0) += count;
        }

        self.validate_success += other.validate_success;
        self.validate_failures += other.validate_failures;
        self.setup_total += other.setup_total;
        self.validate_total += other.validate_total;
        self.teardown_total += other.teardown_total;
        self.teardown_success += other.teardown_success;
        self.teardown_failures += other.teardown_failures;
        self.interrupted |= other.interrupted;
        // 合并发生在所有轮次结束之后:汇合后的 Recorder 不再上报进度,
        // 避免误用时对共享计数二次累加
        #[cfg(feature = "tui")]
        {
            self.progress = None;
        }
    }

    /// 标记一轮 `run` 开始(实时进度的在途请求计数)。
    #[cfg(feature = "tui")]
    pub fn begin_round(&mut self) {
        if let Some(progress) = &self.progress {
            progress.begin_round();
        }
    }

    /// 标记一轮 `run` 结束。
    #[cfg(feature = "tui")]
    pub fn end_round(&mut self) {
        if let Some(progress) = &self.progress {
            progress.end_round();
        }
    }

    /// 标记进入任务级 `setup` 阶段。
    #[cfg(feature = "tui")]
    pub fn begin_setup(&mut self) {
        if let Some(progress) = &self.progress {
            progress.begin_setup();
        }
    }

    /// 标记退出任务级 `setup` 阶段。
    #[cfg(feature = "tui")]
    pub fn end_setup(&mut self) {
        if let Some(progress) = &self.progress {
            progress.end_setup();
        }
    }

    pub fn record_duration(&mut self, duration: Duration) {
        self.histogram
            .record(duration.as_micros() as u64)
            .expect("duration is out of histogram range");
        self.total += duration;
        #[cfg(feature = "tui")]
        if let Some(progress) = &self.progress {
            progress.round_recorded();
        }
    }

    pub fn record_result<T, E>(&mut self, result: &std::result::Result<T, E>)
    where
        E: ScenarioError,
    {
        match result {
            Ok(_) => self.success += 1,
            Err(err) => {
                self.log_internal(format!("{err:?}"));
                if err.is_timeout() {
                    // 超时:被测系统过载信号,不计入失败
                    self.timeouts += 1;
                    #[cfg(feature = "tui")]
                    if let Some(progress) = &self.progress {
                        progress.timeout_recorded();
                    }
                } else {
                    self.failures += 1;
                    #[cfg(feature = "tui")]
                    if let Some(progress) = &self.progress {
                        progress.result_recorded(false);
                    }
                }
                self.error_map
                    .entry(err.kind())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }
        }
        #[cfg(feature = "tui")]
        if let Some(progress) = &self.progress {
            progress.result_recorded(result.is_ok());
        }
    }

    pub fn record_validate(&mut self, validate_result: bool) {
        match validate_result {
            true => {
                self.validate_success += 1;
            }
            false => {
                self.validate_failures += 1;
            }
        }
        #[cfg(feature = "tui")]
        if let Some(progress) = &self.progress {
            progress.validate_recorded(validate_result);
        }
    }

    /// 记录一次 validate 失败,并把失败类目计入错误明细(Errors 段)。
    /// `kind` 为失败类目:validate 返回 `Err` 时传其 `kind()`,
    /// 返回 `Ok(false)` 时传统一类目 `"validate"`。
    pub fn record_validate_failure(&mut self, kind: Cow<'static, str>) {
        self.validate_failures += 1;
        self.error_map
            .entry(kind)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        #[cfg(feature = "tui")]
        if let Some(progress) = &self.progress {
            progress.validate_recorded(false);
        }
    }

    /// 框架内部日志门面:TUI 激活时进日志缓冲(不再打扰终端),
    /// 否则照常落 tracing。
    pub(crate) fn log_internal(&self, message: String) {
        #[cfg(feature = "tui")]
        if self.progress.as_ref().is_some_and(|p| p.push_log(&message)) {
            return;
        }
        warn!("{message}");
    }

    /// 记录一次 setup 尝试的耗时:超时退避后的每次重试都单独计入。
    pub fn record_setup_duration(&mut self, duration: Duration) {
        self.setup_total += duration;
    }

    /// 记录一次 validate 的耗时。
    pub fn record_validate_duration(&mut self, duration: Duration) {
        self.validate_total += duration;
    }

    /// 记录一次 teardown 的耗时。
    pub fn record_teardown_duration(&mut self, duration: Duration) {
        self.teardown_total += duration;
    }

    /// 记录一次 teardown 结果。失败不加剧 `failures`/`timeouts`(那两项只反映
    /// 荷载路径的成败),故单列计数;错误类目由 [`Self::record_teardown_error`] 记录。
    pub fn record_teardown(&mut self, ok: bool) {
        if ok {
            self.teardown_success += 1;
        } else {
            self.teardown_failures += 1;
        }
        #[cfg(feature = "tui")]
        if let Some(progress) = &self.progress {
            progress.teardown_recorded(ok);
        }
    }

    /// 记录一次 teardown 失败的类目,自动加 `teardown:` 前缀,
    /// 便于在错误明细里与 run 侧失败区分。
    pub fn record_teardown_error(&mut self, kind: Cow<'static, str>) {
        let key: Cow<'static, str> = format!("teardown:{kind}").into();
        self.error_map
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }

    /// 标记该任务因优雅关闭提前结束(计入报告)。
    pub fn record_interrupted(&mut self) {
        self.interrupted = true;
    }
}
