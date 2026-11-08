//! Real process-global tracing subscriber acceptance with isolated child
//! processes. No collector, credentials, or process-wide environment races.
use std::process::{Command, Output};

const MARKER: &str = "FIDUCIA_LOG_LEVEL_E2E_CHILD";

#[test]
fn child_log_filter_entry() {
    if std::env::var(MARKER).as_deref() != Ok("1") {
        return;
    }
    let guard = fiducia_telemetry::init("telemetry-filter-e2e");
    assert!(!guard.otlp_enabled(), "test must never require a collector");
    tracing::debug!(check = "debug", "debug-probe-20261105");
    tracing::info!(check = "info", "info-probe-20261105");
    tracing::warn!(check = "warn", "warn-probe-20261105");
    tracing::error!(check = "error", "error-probe-20261105");
    fiducia_telemetry::shutdown();
    drop(guard);
}

fn child(filter: &str) -> Output {
    Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "child_log_filter_entry", "--nocapture"])
        .env(MARKER, "1")
        .env("RUST_LOG", filter)
        .env("FIDUCIA_LOG_FORMAT", "json")
        .env("NO_COLOR", "1")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .output()
        .expect("run independently scoped tracer process")
}

fn assert_levels(filter: &str, expected: &[&str]) {
    let output = child(filter);
    assert!(
        output.status.success(),
        "child failed for filter {filter}: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();
    for label in ["debug", "info", "warn", "error"] {
        let probe = format!("{label}-probe-20261105");
        let found = events.iter().any(|event| event["message"] == probe);
        assert_eq!(
            found,
            expected.contains(&label),
            "RUST_LOG={filter}: unexpected {label} event in {events:?}"
        );
    }
    let startup = events
        .iter()
        .find(|event| event["message"] == "telemetry initialized");
    assert_eq!(
        startup.is_some(),
        expected.contains(&"info"),
        "startup logging must honor subscriber EnvFilter"
    );
    if let Some(startup) = startup {
        assert_eq!(startup["service.name"], "telemetry-filter-e2e");
        assert_eq!(startup["otel.trace_exporter"], false);
        assert_eq!(startup["otel.metric_exporter"], false);
    }
    assert!(
        !stdout.contains("\u{1b}["),
        "machine-readable event stream must not contain ANSI formatting"
    );
}

#[test]
fn info_filter_logs_application_info_warnings_and_errors_but_not_debug() {
    assert_levels("info", &["info", "warn", "error"]);
}

#[test]
fn warn_filter_suppresses_startup_and_application_info_but_keeps_errors() {
    assert_levels("warn", &["warn", "error"]);
}

#[test]
fn error_filter_only_exposes_application_errors() {
    assert_levels("error", &["error"]);
}
