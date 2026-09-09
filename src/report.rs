use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::{Display, Formatter, Result, Write as _},
    time::Duration,
};

use crate::recorder::Recorder;

#[derive(Debug)]
pub struct ScenarioReport {
    pub name: Cow<'static, str>,
    /// 该场景的执行并发度(Manager 分配的任务数)。
    pub concurrency: u64,
    pub times: u64,
    pub total: Duration,
    pub avg: Duration,
    pub min: Duration,
    pub max: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,

    pub validate_success: u64,
    pub validate_failures: u64,

    pub success: u64,
    pub failures: u64,
    pub error_map: HashMap<Cow<'static, str>, u64>,
}

impl ScenarioReport {
    pub fn from_recorder(
        name: impl Into<Cow<'static, str>>,
        mut recorder: Recorder,
        concurrency: u64,
    ) -> Self {
        Self {
            name: name.into(),
            concurrency,
            times: recorder.times(),
            total: recorder.total(),
            avg: recorder.avg(),
            min: recorder.min(),
            max: recorder.max(),
            p50: recorder.p50(),
            p95: recorder.p95(),
            p99: recorder.p99(),

            validate_success: recorder.validate_success,
            validate_failures: recorder.validate_failures,

            success: recorder.success,
            failures: recorder.failures,
            error_map: recorder.error_map,
        }
    }
}

impl Display for ScenarioReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        writeln!(f, "Scenario: {}", self.name)?;
        writeln!(f, "  Times: {}", self.times)?;
        writeln!(f, "  Concurrency: {}", self.concurrency)?;
        writeln!(f, "  Total: {:?}", self.total)?;
        writeln!(f, "  Min:  {:?}", self.min)?;
        writeln!(f, "  Avg:  {:?}", self.avg)?;
        writeln!(f, "  Max:  {:?}", self.max)?;
        writeln!(f, "  P50:  {:?}", self.p50)?;
        writeln!(f, "  P95:  {:?}", self.p95)?;
        writeln!(f, "  P99:  {:?}", self.p99)?;

        writeln!(f, "  Validate Success:  {}", self.validate_success)?;
        writeln!(f, "  Validate Failures: {}", self.validate_failures)?;

        writeln!(f, "  Success: {}", self.success)?;
        writeln!(f, "  Failures: {}", self.failures)?;

        if !self.error_map.is_empty() {
            writeln!(f, "  Errors:")?;

            for (kind, count) in &self.error_map {
                writeln!(f, "    {kind}: {count}")?;
            }
        }

        Ok(())
    }
}

/// 可视化报告渲染选项
#[derive(Debug, Clone, Copy)]
pub struct ReportOptions {
    /// 是否启用 ANSI 颜色。默认关闭,可用 `report_with` 显式开启
    pub color: bool,
    /// 条形图宽度(字符数)
    pub bar_width: usize,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            color: false,
            bar_width: 20,
        }
    }
}

/// 可视化报告入口:让单个场景与多个场景统一支持 `.report()` / `.report_with()` 调用。
///
/// `Vec<ScenarioReport>` 通过本 crate 的 `impl Report for Vec<ScenarioReport>`
/// 直接可用,`&[ScenarioReport]` 亦可。
pub trait Report {
    /// 渲染为可视化文本(使用默认选项:颜色关闭)
    fn report(&self) -> String;

    /// 以自定义选项渲染为可视化文本
    fn report_with(&self, options: ReportOptions) -> String;

    /// 渲染为带 ANSI 颜色的可视化文本(条形图宽度使用默认值)
    fn report_with_color(&self) -> String {
        self.report_with(ReportOptions {
            color: true,
            ..ReportOptions::default()
        })
    }
}

impl Report for ScenarioReport {
    fn report(&self) -> String {
        render_single(self, &ReportOptions::default())
    }

    fn report_with(&self, options: ReportOptions) -> String {
        render_single(self, &options)
    }
}

impl Report for [ScenarioReport] {
    fn report(&self) -> String {
        render_many(self, &ReportOptions::default())
    }

    fn report_with(&self, options: ReportOptions) -> String {
        render_many(self, &options)
    }
}

impl Report for Vec<ScenarioReport> {
    fn report(&self) -> String {
        self.as_slice().report()
    }

    fn report_with(&self, options: ReportOptions) -> String {
        self.as_slice().report_with(options)
    }
}

// ---------- 渲染内核 ----------

fn render_single(r: &ScenarioReport, o: &ReportOptions) -> String {
    let mut out = String::new();

    writeln!(
        out,
        "{}",
        paint("============= Scenario =============", "1;36", o.color)
    )
    .unwrap();
    writeln!(out, "{}", paint(&format!("  {}", r.name), "1", o.color)).unwrap();
    writeln!(
        out,
        "{}",
        paint("====================================", "1;36", o.color)
    )
    .unwrap();

    let success_rate = rate_of(r.success, r.times);
    let fail_rate = rate_of(r.failures, r.times);

    writeln!(out, "Requests : {}", r.times).unwrap();
    writeln!(out, "Concurrent: {}", r.concurrency).unwrap();
    writeln!(out, "Total    : {}", fmt_duration(r.total)).unwrap();
    writeln!(
        out,
        "Success  : {} ({:.1}%) {}",
        r.success,
        success_rate * 100.0,
        paint(rate_label(success_rate), rate_color(success_rate), o.color),
    )
    .unwrap();
    writeln!(out, "Failures : {} ({:.1}%)", r.failures, fail_rate * 100.0,).unwrap();
    writeln!(
        out,
        "Validate : {} ok / {} fail",
        r.validate_success, r.validate_failures,
    )
    .unwrap();

    if r.times > 0 {
        let max_ns = r.max.as_nanos().max(1) as f64;
        writeln!(out).unwrap();
        writeln!(out, "Latency (relative to max):").unwrap();
        for (label, d) in [
            ("Min", r.min),
            ("Avg", r.avg),
            ("Max", r.max),
            ("P50", r.p50),
            ("P95", r.p95),
            ("P99", r.p99),
        ] {
            let frac = d.as_nanos() as f64 / max_ns;
            writeln!(
                out,
                "  {:<4} {} {}",
                label,
                bar(frac, o.bar_width),
                fmt_duration(d),
            )
            .unwrap();
        }
    }

    if !r.error_map.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "Errors:").unwrap();

        let mut errs: Vec<_> = r.error_map.iter().collect();
        errs.sort_by(|a, b| b.1.cmp(a.1));
        let denom = r.failures.max(1) as f64;
        for (kind, count) in errs {
            let frac = *count as f64 / denom;
            writeln!(
                out,
                "  {:<24} {} {} ({:.1}%)",
                kind,
                bar(frac, o.bar_width),
                count,
                frac * 100.0,
            )
            .unwrap();
        }
    }

    out
}

fn render_many(reports: &[ScenarioReport], o: &ReportOptions) -> String {
    let mut out = String::new();

    if reports.is_empty() {
        return paint("No reports.", "33", o.color);
    }

    // ---- 总览 ----
    let total_times: u64 = reports.iter().map(|r| r.times).sum();
    let total_success: u64 = reports.iter().map(|r| r.success).sum();
    let total_failures: u64 = reports.iter().map(|r| r.failures).sum();
    let total_validate_success: u64 = reports.iter().map(|r| r.validate_success).sum();
    let total_validate_failures: u64 = reports.iter().map(|r| r.validate_failures).sum();
    let total_total: Duration = reports.iter().map(|r| r.total).sum();

    // 并发度:各场景一致时显示单值,不一致时显示范围
    let first_concurrency = reports[0].concurrency;
    let concurrency_label = if reports.iter().all(|r| r.concurrency == first_concurrency) {
        first_concurrency.to_string()
    } else {
        let min = reports.iter().map(|r| r.concurrency).min().unwrap();
        let max = reports.iter().map(|r| r.concurrency).max().unwrap();
        format!("{min}-{max}")
    };
    let avg: Duration = if total_times > 0 {
        let num: u128 = reports
            .iter()
            .map(|r| r.avg.as_nanos() * r.times as u128)
            .sum();
        Duration::from_nanos((num / total_times as u128) as u64)
    } else {
        Duration::ZERO
    };
    let total_rate = rate_of(total_success, total_times);

    writeln!(
        out,
        "{}",
        paint("============== Summary ==============", "1;36", o.color)
    )
    .unwrap();
    writeln!(out, "Scenarios : {}", reports.len()).unwrap();
    writeln!(out, "Concurrent : {concurrency_label}").unwrap();
    writeln!(out, "Requests  : {}", total_times).unwrap();
    writeln!(out, "Total     : {}", fmt_duration(total_total)).unwrap();
    writeln!(out, "Avg       : {}", fmt_duration(avg)).unwrap();
    writeln!(
        out,
        "Success   : {} ({:.1}%) {}",
        total_success,
        total_rate * 100.0,
        paint(rate_label(total_rate), rate_color(total_rate), o.color),
    )
    .unwrap();
    writeln!(out, "Failures  : {}", total_failures).unwrap();
    writeln!(
        out,
        "Validate  : {} ok / {} fail",
        total_validate_success, total_validate_failures,
    )
    .unwrap();
    writeln!(
        out,
        "Overall   : {} {}",
        bar(total_rate, o.bar_width),
        paint(rate_label(total_rate), rate_color(total_rate), o.color),
    )
    .unwrap();

    // ---- 逐场景表格 ----
    writeln!(out).unwrap();
    writeln!(out, "{}", paint("---- Per-scenario ----", "1;36", o.color)).unwrap();

    let name_w = reports
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(0)
        .max(4);

    let header = format!(
        "{:<name_w$} {:>8} {:>10} {:>10} {:>8} {:>10} {:>10} {:>12}",
        "Name",
        "Times",
        "Success",
        "Failures",
        "Rate",
        "Avg",
        "P95",
        "Validate",
        name_w = name_w,
    );
    writeln!(out, "{}", paint(&header, "36", o.color)).unwrap();

    for r in reports {
        let rate = rate_of(r.success, r.times);
        writeln!(
            out,
            "{:<name_w$} {:>8} {:>10} {:>10} {:>8} {:>10} {:>10} {:>12}",
            r.name,
            r.times,
            r.success,
            r.failures,
            format!("{:.1}%", rate * 100.0),
            fmt_duration(r.avg),
            fmt_duration(r.p95),
            format!("{}/{}", r.validate_success, r.validate_failures),
            name_w = name_w,
        )
        .unwrap();
    }

    // ---- 错误明细 ----
    let err_rows: Vec<_> = reports.iter().filter(|r| !r.error_map.is_empty()).collect();
    if !err_rows.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "{}", paint("---- Errors ----", "1;36", o.color)).unwrap();

        for r in err_rows {
            writeln!(out, "{}", paint(&format!("  {}.", r.name), "1", o.color)).unwrap();

            let mut errs: Vec<_> = r.error_map.iter().collect();
            errs.sort_by(|a, b| b.1.cmp(a.1));
            let denom = r.failures.max(1) as f64;
            for (kind, count) in errs {
                let frac = *count as f64 / denom;
                writeln!(
                    out,
                    "    {:<24} {} {} ({:.1}%)",
                    kind,
                    bar(frac, o.bar_width),
                    count,
                    frac * 100.0,
                )
                .unwrap();
            }
        }
    }

    out
}

// ---------- 渲染辅助 ----------

/// 是否启用颜色的判定:由 `ReportOptions::color` 显式控制。
/// 默认关闭颜色,保证输出可预测、可直接写入日志/文件;
/// 需要彩色时用 `report_with(ReportOptions { color: true, .. })`。
fn paint(s: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn rate_of(success: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        success as f64 / total as f64
    }
}

fn rate_color(rate: f64) -> &'static str {
    if rate >= 0.99 {
        "32" // 绿
    } else if rate >= 0.90 {
        "33" // 黄
    } else {
        "31" // 红
    }
}

fn rate_label(rate: f64) -> &'static str {
    if rate >= 0.99 {
        "OK"
    } else if rate >= 0.90 {
        "WARN"
    } else {
        "BAD"
    }
}

/// 生成 ASCII 条形图,`fraction` 为 0.0 ~ 1.0
fn bar(fraction: f64, width: usize) -> String {
    let width = width.max(1);
    let filled = (fraction.clamp(0.0, 1.0) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// 可读化 Duration,按量级自适应单位
fn fmt_duration(d: Duration) -> String {
    let ns = d.as_nanos();
    if ns >= 1_000_000_000 {
        format!("{:.2}s", ns as f64 / 1e9)
    } else if ns >= 1_000_000 {
        format!("{:.2}ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.2}µs", ns as f64 / 1e3)
    } else {
        format!("{ns}ns")
    }
}
