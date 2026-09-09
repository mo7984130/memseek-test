use std::{borrow::Cow, collections::HashMap, time::Duration};

use memseek_test::{Report, ReportOptions, ScenarioReport};

fn sample_report(name: &'static str, times: u64, success: u64, failures: u64) -> ScenarioReport {
    let mut error_map = HashMap::new();
    if failures > 0 {
        error_map.insert(Cow::Borrowed("timeout"), failures);
    }
    ScenarioReport {
        name: Cow::Borrowed(name),
        times,
        total: Duration::from_millis(1200),
        avg: Duration::from_millis(12),
        min: Duration::from_micros(300),
        max: Duration::from_millis(95),
        p50: Duration::from_millis(8),
        p95: Duration::from_millis(40),
        p99: Duration::from_millis(80),
        validate_success: success,
        validate_failures: failures,
        success,
        failures,
        error_map,
    }
}

#[test]
fn single_report_contains_core_stats() {
    let r = sample_report("login", 100, 98, 2);
    let text = r.report();

    assert!(text.contains("login"));
    assert!(text.contains("Requests : 100"));
    assert!(text.contains("98 (98.0%)"));
    assert!(text.contains("2 (2.0%)"));
    assert!(text.contains("Latency"));
    assert!(text.contains("P95"));
    assert!(text.contains("timeout"));
}

#[test]
fn single_report_rate_colored_when_requested() {
    let r = sample_report("ok", 1000, 999, 1);
    // 99.9% >= 0.99,应为绿色 32
    let colored = r.report_with(ReportOptions {
        color: true,
        bar_width: 20,
    });
    assert!(colored.contains("\x1b[32mOK\x1b[0m"));

    // 默认(测试环境 stdout 非 TTY)不含 ANSI 转义
    let plain = r.report();
    assert!(!plain.contains("\x1b["));

    // report_with_color 便捷方法产出 ANSI 颜色
    assert!(r.report_with_color().contains("\x1b[32mOK\x1b[0m"));
}

#[test]
fn vec_report_has_summary_and_table() {
    let reports = vec![
        sample_report("login", 1000, 998, 2),
        sample_report("order", 500, 400, 100),
    ];
    let text = reports.report();

    assert!(text.contains("Summary"));
    assert!(text.contains("Scenarios : 2"));
    assert!(text.contains("Requests  : 1500"));
    assert!(text.contains("Success   : 1398 (93.2%)"));
    assert!(text.contains("login"));
    assert!(text.contains("order"));
    assert!(text.contains("Per-scenario"));
}

#[test]
fn vec_report_empty() {
    let reports: Vec<ScenarioReport> = Vec::new();
    assert!(reports.report().contains("No reports"));
}

#[test]
fn report_with_works_on_vec_and_slice() {
    let reports = vec![
        sample_report("login", 100, 98, 2),
        sample_report("order", 50, 40, 10),
    ];
    let opts = ReportOptions {
        color: true,
        bar_width: 20,
    };

    // Vec 直接调用 report_with
    assert!(reports.report_with(opts).contains("Summary"));
    // slice 调用 report_with
    assert!(reports.as_slice().report_with(opts).contains("Summary"));
}

#[test]
fn duration_formatting_scales() {
    // 通过 report_with 间接验证可读化单位
    let mut r = sample_report("dur", 10, 10, 0);
    r.total = Duration::from_secs(3);
    r.avg = Duration::from_millis(250);
    r.max = Duration::from_millis(500);
    r.p95 = Duration::from_micros(1500);

    let text = r.report_with(ReportOptions {
        color: false,
        bar_width: 20,
    });
    assert!(text.contains("3.00s"));
    assert!(text.contains("250.00ms"));
    assert!(text.contains("1.50ms"));
}
