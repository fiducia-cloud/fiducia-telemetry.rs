//! Nested tracing spans must survive the production JSON subscriber while
//! stdout remains parseable and no collector is configured.
use std::process::{Command, Output};

const CHILD: &str = "FIDUCIA_TELEMETRY_NESTED_SPANS_E2E_CHILD";

#[test]
fn emit_nested_span_child() {
    if std::env::var(CHILD).as_deref() != Ok("1") {
        return;
    }
    let guard = fiducia_telemetry::init("tracing-span-wire-e2e");
    assert!(!guard.otlp_enabled());
    let parent = tracing::info_span!("parent", tenant = "synthetic-tenant-a");
    let _parent = parent.enter();
    tracing::info!(request = "request-a", "parent-probe-20261105");
    {
        let child = tracing::info_span!("child", request_id = "test-42");
        let _child = child.enter();
        tracing::warn!(result = "bounded", "child-probe-20261105");
    }
    tracing::error!(status = "done", "parent-after-child-20261105");
    fiducia_telemetry::shutdown();
    drop(guard);
}

fn child_run(filter: &str) -> Output {
    Command::new(std::env::current_exe().expect("running test executable"))
        .args(["--exact", "emit_nested_span_child", "--nocapture"])
        .env(CHILD, "1")
        .env("FIDUCIA_LOG_FORMAT", "json")
        .env("RUST_LOG", filter)
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env("NO_COLOR", "1")
        .output()
        .expect("independent tracing process")
}

fn records(output: &Output) -> Vec<serde_json::Value> {
    assert!(
        output.status.success(),
        "child failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect()
}

#[test]
fn nested_span_event_keeps_both_ancestry_entries_and_current_child() {
    let values = records(&child_run("info"));
    let child = values
        .iter()
        .find(|event| event["message"] == "child-probe-20261105")
        .expect("child warning event exists");
    let ancestry = child["spans"]
        .as_array()
        .expect("production JSON subscriber must include span ancestry");
    let names: Vec<&str> = ancestry
        .iter()
        .filter_map(|span| span["name"].as_str())
        .collect();
    assert_eq!(names, vec!["parent", "child"]);
    assert_eq!(child["span"]["name"], "child");
    let after = values
        .iter()
        .find(|event| event["message"] == "parent-after-child-20261105")
        .expect("parent event after nested scope");
    let after_names: Vec<&str> = after["spans"]
        .as_array()
        .expect("parent still visible")
        .iter()
        .filter_map(|span| span["name"].as_str())
        .collect();
    assert_eq!(after_names, vec!["parent"]);
    assert_eq!(after["span"]["name"], "parent");
}

#[test]
fn warn_filter_suppresses_parent_info_but_preserves_nested_warning_and_error() {
    let values = records(&child_run("warn"));
    assert!(values.iter().all(|event| event["message"] != "parent-probe-20261105"));
    assert!(values.iter().any(|event| event["message"] == "child-probe-20261105"));
    assert!(values.iter().any(|event| event["message"] == "parent-after-child-20261105"));
    assert!(values.iter().all(|event| event["otel.trace_exporter"] != true));
    assert!(values.iter().all(|event| event["otel.metric_exporter"] != true));
}
