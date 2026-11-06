//! Process-isolated telemetry acceptance: each case owns a fresh global tracing
//! subscriber, and none may contact a collector or transmit credentials.
use std::process::{Command, Output};

const CHILD_MARKER: &str = "FIDUCIA_TELEMETRY_DEEP_E2E_CHILD";

#[test]
fn child_entry() {
    if std::env::var(CHILD_MARKER).as_deref() != Ok("1") {
        return;
    }
    let guard = fiducia_telemetry::init("telemetry-fallback-e2e");
    assert!(
        !guard.otlp_enabled(),
        "unset or whitespace-only OTLP settings must not create exporters"
    );
    tracing::info!(probe.case = "isolated", "emitted after telemetry startup");
    fiducia_telemetry::shutdown();
    drop(guard);
}

fn invoke_child(endpoint: Option<&str>, formats: &[(&str, &str)]) -> Output {
    let runner = std::env::current_exe().expect("current integration test");
    let mut child = Command::new(runner);
    child
        .args(["--exact", "child_entry", "--nocapture"])
        .env(CHILD_MARKER, "1")
        .env("RUST_LOG", "info")
        .env("NO_COLOR", "1")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("FIDUCIA_LOG_FORMAT")
        .env_remove("OTEL_LOG_FORMAT")
        .env_remove("LOG_FORMAT");
    if let Some(endpoint) = endpoint {
        child.env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint);
    }
    for (key, value) in formats {
        child.env(key, value);
    }
    child.output().expect("run isolated tracing subprocess")
}

fn assert_healthy_child(output: &Output) {
    assert!(
        output.status.success(),
        "telemetry child failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn startup_record(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["message"] == "telemetry initialized")
        .unwrap_or_else(|| panic!("missing machine-readable startup: {stdout}"))
}

#[test]
fn absent_and_whitespace_endpoints_produce_identical_fail_closed_startup() {
    for endpoint in [None, Some(""), Some("  \t\n  ")] {
        let output = invoke_child(endpoint, &[("FIDUCIA_LOG_FORMAT", "json")]);
        assert_healthy_child(&output);
        let record = startup_record(&output);
        assert_eq!(record["service.name"], "telemetry-fallback-e2e");
        assert_eq!(record["otel.trace_exporter"], false);
        assert_eq!(record["otel.metric_exporter"], false);
        assert_eq!(record["log.format"], "json");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("emitted after telemetry startup"),
            "the no-collector path must still emit application events"
        );
        assert!(
            !stdout.contains("one or more OTLP exporters failed"),
            "disabled telemetry is not a collector failure"
        );
    }
}

#[test]
fn fleet_specific_format_overrides_generic_and_legacy_env_in_child_process() {
    let output = invoke_child(
        None,
        &[
            ("FIDUCIA_LOG_FORMAT", "JSON"),
            ("OTEL_LOG_FORMAT", "text"),
            ("LOG_FORMAT", "text"),
        ],
    );
    assert_healthy_child(&output);
    let startup = startup_record(&output);
    assert_eq!(startup["log.format"], "json");
    assert_eq!(startup["otel.trace_exporter"], false);
    assert_eq!(startup["otel.metric_exporter"], false);
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("\u{1b}["),
        "structured CI events must never include ANSI escape control sequences"
    );
}

#[test]
fn generic_format_controls_fallback_when_fleet_setting_is_absent() {
    let output = invoke_child(
        Some(" \t "),
        &[("OTEL_LOG_FORMAT", "text"), ("LOG_FORMAT", "json")],
    );
    assert_healthy_child(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("telemetry initialized"));
    assert!(text.contains("emitted after telemetry startup"));
    assert!(
        !text.lines().any(|line| line.contains("\"message\":\"telemetry initialized\"")),
        "the explicit OTEL text setting must not be overridden by legacy JSON"
    );
}
