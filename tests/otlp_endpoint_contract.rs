//! Black-box contracts for the public telemetry initialization boundary.
//!
//! These cases intentionally run in subprocesses because `tracing` installs a
//! process-global subscriber exactly once.

use std::process::Command;

const CHILD_MARKER: &str = "FIDUCIA_TELEMETRY_CONTRACT_CHILD";

#[test]
fn child_init_helper() {
    if std::env::var(CHILD_MARKER).as_deref() != Ok("1") {
        return;
    }

    let guard = fiducia_telemetry::init("otlp-endpoint-contract");
    assert!(
        !guard.otlp_enabled(),
        "a whitespace-only OTLP endpoint must behave exactly like an unset endpoint"
    );
    fiducia_telemetry::shutdown();
    drop(guard);
}

#[test]
fn whitespace_only_otlp_endpoint_falls_back_to_stdout_without_exporters() {
    let executable = std::env::current_exe().expect("integration-test executable");
    let output = Command::new(executable)
        .args([
            "--exact",
            "child_init_helper",
            "--nocapture",
        ])
        .env(CHILD_MARKER, "1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", " \t\n ")
        .env("FIDUCIA_LOG_FORMAT", "json")
        .env_remove("RUST_LOG")
        .output()
        .expect("spawn telemetry contract child");

    assert!(
        output.status.success(),
        "child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let startup = events
        .iter()
        .find(|event| event["message"] == "telemetry initialized")
        .unwrap_or_else(|| panic!("startup event missing from stdout:\n{stdout}"));

    assert_eq!(startup["service.name"], "otlp-endpoint-contract");
    assert_eq!(startup["otel.trace_exporter"], false);
    assert_eq!(startup["otel.metric_exporter"], false);
    assert!(
        !stdout.contains("one or more OTLP exporters failed to initialize"),
        "blank configuration is disabled, not an exporter failure"
    );
}
