//! Shared telemetry setup for every fiducia.cloud Rust service.
//!
//! One call — [`init`] — wires up `tracing` for the whole process:
//!   * always: JSON structured logs to stdout with `RUST_LOG`/`EnvFilter`
//!     filtering (`FIDUCIA_LOG_FORMAT=text` for local human-readable logs);
//!   * when `OTEL_EXPORTER_OTLP_ENDPOINT` is set: OpenTelemetry **OTLP** trace
//!     and metric exporters (gRPC), tagged with service and deployment resource
//!     attributes. The collector can route metrics to Prometheus and traces to
//!     its configured tracing backend;
//!
//! Services normally export OTLP to a local OpenTelemetry collector. With no
//! endpoint configured (local dev), telemetry degrades to stdout-only logging.
//!
//! Call once at the top of an async `main` (a Tokio runtime must be active for
//! the batch exporter):
//!
//! ```no_run
//! #[tokio::main]
//! async fn main() {
//!     let _telemetry = fiducia_telemetry::init("fiducia-node");
//!     // ...
//! }
//! ```

use opentelemetry::trace::noop::NoopTracerProvider;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    metrics::{PeriodicReader, SdkMeterProvider},
    trace::{SdkTracerProvider, Tracer},
    Resource,
};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Owns the OpenTelemetry providers so final trace and metric batches can be
/// flushed when the service exits.
#[must_use = "keep the telemetry guard alive for the lifetime of the service"]
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl TelemetryGuard {
    /// Whether at least one OTLP signal exporter initialized successfully.
    pub fn otlp_enabled(&self) -> bool {
        self.tracer_provider.is_some() || self.meter_provider.is_some()
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        let tracer_provider = self.tracer_provider.take();
        let meter_provider = self.meter_provider.take();
        if tracer_provider.is_none() && meter_provider.is_none() {
            return;
        }

        if std::thread::spawn(move || {
            if let Some(provider) = meter_provider {
                let _ = provider.shutdown();
            }
            if let Some(provider) = tracer_provider {
                let _ = provider.shutdown();
            }
        })
        .join()
        .is_err()
        {
            eprintln!("telemetry: shutdown flush panicked; final batches may be incomplete");
        }
    }
}

/// Initialize structured logs plus optional OTLP trace and metric export for
/// `service_name`.
///
/// Keep the returned guard alive for the process lifetime. Services normally
/// send OTLP to the local collector; the gateway routes logs to Loki and
/// metrics to Prometheus-compatible storage.
pub fn init(service_name: &str) -> TelemetryGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let log_format = LogFormat::from_env();
    let resource = resource(service_name);
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty());

    let (tracer_provider, tracer) = endpoint
        .as_deref()
        .and_then(|endpoint| build_tracer_provider(endpoint, resource.clone()).ok())
        .map_or((None, None), |(provider, tracer)| {
            global::set_tracer_provider(provider.clone());
            (Some(provider), Some(tracer))
        });
    let meter_provider = endpoint
        .as_deref()
        .and_then(|endpoint| build_meter_provider(endpoint, resource).ok());
    if let Some(provider) = meter_provider.as_ref() {
        global::set_meter_provider(provider.clone());
    }

    match tracer {
        Some(tracer) => install_otlp_subscriber(filter, log_format, tracer),
        None => install_stdout_subscriber(filter, log_format),
    }
    record_service_start(service_name);

    tracing::info!(
        service.name = service_name,
        log.format = log_format.as_str(),
        log.pipeline = "stdout-json-to-collector-to-loki",
        otel.trace_exporter = tracer_provider.is_some(),
        otel.metric_exporter = meter_provider.is_some(),
        metric.pipeline = "otlp-to-collector-to-prometheus",
        "telemetry initialized"
    );
    if endpoint.is_some() && (tracer_provider.is_none() || meter_provider.is_none()) {
        // Exporter errors can echo a credential-bearing endpoint, so retain the
        // useful signal without logging the error or endpoint text.
        tracing::error!("one or more OTLP exporters failed to initialize");
    }

    TelemetryGuard {
        tracer_provider,
        meter_provider,
    }
}

fn build_tracer_provider(
    endpoint: &str,
    resource: Resource,
) -> Result<(SdkTracerProvider, Tracer), ()> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|_| ())?;
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();
    let tracer = provider.tracer("fiducia");
    Ok((provider, tracer))
}

fn build_meter_provider(endpoint: &str, resource: Resource) -> Result<SdkMeterProvider, ()> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|_| ())?;
    let reader = PeriodicReader::builder(exporter).build();
    Ok(SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource)
        .build())
}

fn record_service_start(service_name: &str) {
    let starts = global::meter("fiducia-telemetry")
        .u64_counter("fiducia.service.starts")
        .with_description("Number of Fiducia service process starts")
        .with_unit("{start}")
        .build();
    starts.add(
        1,
        &[KeyValue::new("service.name", service_name.to_string())],
    );
}

/// Legacy global-provider reset. New services must keep the guard returned by
/// [`init`] alive and let it flush traces and metrics on drop. OpenTelemetry
/// 0.32 no longer exposes a type-erased global shutdown operation, so this
/// compatibility hook prevents new spans after legacy callers finish while
/// the owned guard remains the authoritative flush path.
pub fn shutdown() {
    global::set_tracer_provider(NoopTracerProvider::new());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogFormat {
    Json,
    Text,
}

impl LogFormat {
    fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// The precedence chain with the environment injected, so it is
    /// unit-testable without process-global env mutation (which races across
    /// parallel tests) — same pattern as [`resource_attributes`].
    fn from_env_with(get: impl Fn(&str) -> Option<String>) -> Self {
        Self::from_value(
            &get("FIDUCIA_LOG_FORMAT")
                .or_else(|| get("OTEL_LOG_FORMAT"))
                // Compatibility for services that predate the shared telemetry
                // crate. Fleet-specific variables above keep precedence.
                .or_else(|| get("LOG_FORMAT"))
                .unwrap_or_else(|| "json".to_string()),
        )
    }

    fn from_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" | "plain" | "pretty" | "compact" => Self::Text,
            _ => Self::Json,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Text => "text",
        }
    }
}

fn resource(service_name: &str) -> Resource {
    Resource::builder_empty()
        .with_attributes(resource_attributes(service_name, |name| {
            std::env::var(name).ok()
        }))
        .build()
}

/// The full resource-attribute assembly, with the environment injected so the
/// precedence and fallback rules are unit-testable without process-global env
/// mutation (which races across parallel tests).
fn resource_attributes(service_name: &str, get: impl Fn(&str) -> Option<String>) -> Vec<KeyValue> {
    let non_empty = |name: &str| get(name).filter(|value| !value.trim().is_empty());
    let first_of = |names: &[&str]| names.iter().find_map(|name| non_empty(name));

    let mut attrs = vec![
        KeyValue::new("service.name", service_name.to_string()),
        KeyValue::new(
            "service.namespace",
            non_empty("OTEL_SERVICE_NAMESPACE").unwrap_or_else(|| "fiducia-cloud".to_string()),
        ),
    ];

    let mut push = |names: &[&str], key: &'static str| {
        if let Some(value) = first_of(names) {
            attrs.push(KeyValue::new(key, value));
        }
    };
    push(
        &["FIDUCIA_DEPLOYMENT_ENV", "DEPLOYMENT_ENV"],
        "deployment.environment",
    );
    push(&["FIDUCIA_CLUSTER"], "fiducia.cluster");
    push(&["FIDUCIA_CLUSTER_ID"], "fiducia.cluster_id");
    push(&["FIDUCIA_CLOUD_PROVIDER"], "cloud.provider");
    push(&["FIDUCIA_CLOUD_REGION"], "cloud.region");
    push(&["POD_NAMESPACE"], "k8s.namespace.name");
    push(&["POD_NAME"], "k8s.pod.name");
    push(&["NODE_NAME"], "k8s.node.name");
    push(&["SERVICE_VERSION"], "service.version");
    // Distinguish replicas of the same service: pod name in k8s, hostname
    // elsewhere. Without this, multi-replica traces collapse into one instance.
    push(&["POD_NAME", "HOSTNAME"], "service.instance.id");

    if let Some(raw) = get("OTEL_RESOURCE_ATTRIBUTES") {
        for (key, value) in otel_resource_attribute_pairs(&raw) {
            attrs.push(KeyValue::new(key, value));
        }
    }

    attrs
}

fn install_otlp_subscriber(filter: EnvFilter, log_format: LogFormat, tracer: Tracer) {
    let result = match log_format {
        LogFormat::Json => tracing_subscriber::registry()
            .with(filter)
            .with(json_log_layer())
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .try_init(),
        LogFormat::Text => tracing_subscriber::registry()
            .with(filter)
            .with(text_log_layer())
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .try_init(),
    };

    if let Err(err) = result {
        eprintln!("telemetry: subscriber init skipped: {err}");
    }
}

fn install_stdout_subscriber(filter: EnvFilter, log_format: LogFormat) {
    let result = match log_format {
        LogFormat::Json => tracing_subscriber::registry()
            .with(filter)
            .with(json_log_layer())
            .try_init(),
        LogFormat::Text => tracing_subscriber::registry()
            .with(filter)
            .with(text_log_layer())
            .try_init(),
    };

    if let Err(err) = result {
        eprintln!("telemetry: subscriber init skipped: {err}");
    }
}

fn json_log_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_ansi(false)
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
}

fn text_log_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(std::env::var("NO_COLOR").is_err())
        .with_target(true)
        .with_span_events(FmtSpan::CLOSE)
}

fn otel_resource_attribute_pairs(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            if !key.is_empty() && !value.is_empty() && !is_sensitive_attribute_key(key) {
                Some((key.to_string(), value.to_string()))
            } else {
                None
            }
        })
        .collect()
}

fn is_sensitive_attribute_key(key: &str) -> bool {
    // Substring match after normalization: over-redacting an operator-supplied
    // resource attribute is harmless; leaking `my_secret_value` or `token_id`
    // (which exact/suffix matching missed) is not.
    let normalized = key.trim().to_ascii_lowercase().replace(['-', '.'], "_");
    [
        "authorization",
        "cookie",
        "password",
        "passwd",
        // Abbreviated / spelled-out password variants the exact words above miss.
        "pwd",
        "passphrase",
        "secret",
        "token",
        "api_key",
        "apikey",
        "credential",
        "bearer",
        "private_key",
        // A signing key is a secret even though it is not a "private_key".
        "signing_key",
        "session",
        "jwt",
        // PII: an address in a resource attribute must not reach the exporter.
        "email",
    ]
    .iter()
    .any(|sensitive| normalized.contains(sensitive))
}

#[cfg(test)]
mod interface_contract_tests {
    use fiducia_interfaces::{LockAcquireManyRequest, ProposeErrorReason};

    #[test]
    fn generated_interfaces_are_importable() {
        let request = LockAcquireManyRequest {
            keys: vec!["orders/42".to_string(), "inventory/sku-7".to_string()],
            holder: Some("worker-a".to_string()),
            ttl_ms: Some(30_000),
            wait: Some(false),
        };

        assert_eq!(request.keys.len(), 2);
        assert!(matches!(
            ProposeErrorReason::NotLeader,
            ProposeErrorReason::NotLeader
        ));
    }

    #[test]
    fn log_format_accepts_human_text_aliases() {
        assert_eq!(super::LogFormat::from_value("text"), super::LogFormat::Text);
        assert_eq!(
            super::LogFormat::from_value(" plain "),
            super::LogFormat::Text
        );
        assert_eq!(
            super::LogFormat::from_value("PRETTY"),
            super::LogFormat::Text
        );
        assert_eq!(
            super::LogFormat::from_value("compact"),
            super::LogFormat::Text
        );
    }

    #[test]
    fn log_format_defaults_unknown_values_to_json() {
        assert_eq!(super::LogFormat::from_value(""), super::LogFormat::Json);
        assert_eq!(super::LogFormat::from_value("json"), super::LogFormat::Json);
        assert_eq!(super::LogFormat::from_value("yaml"), super::LogFormat::Json);
    }

    /// The env precedence chain: FIDUCIA_LOG_FORMAT beats OTEL_LOG_FORMAT
    /// beats the legacy LOG_FORMAT, and with none set the fleet default is
    /// JSON. Driven through an injected getter (no process-global env
    /// mutation), like the resource_attributes tests.
    #[test]
    fn log_format_env_precedence_is_fiducia_then_otel_then_legacy() {
        use std::collections::HashMap;
        let from = |env: &[(&str, &str)]| {
            let map: HashMap<String, String> = env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            super::LogFormat::from_env_with(|name| map.get(name).cloned())
        };

        assert_eq!(
            from(&[
                ("FIDUCIA_LOG_FORMAT", "text"),
                ("OTEL_LOG_FORMAT", "json"),
                ("LOG_FORMAT", "json"),
            ]),
            super::LogFormat::Text,
            "the fleet-specific variable must win over both fallbacks"
        );
        assert_eq!(
            from(&[("OTEL_LOG_FORMAT", "text"), ("LOG_FORMAT", "json")]),
            super::LogFormat::Text,
            "OTEL_LOG_FORMAT must win over the legacy variable"
        );
        assert_eq!(
            from(&[("LOG_FORMAT", "text")]),
            super::LogFormat::Text,
            "the legacy variable still applies when nothing else is set"
        );
        assert_eq!(
            from(&[]),
            super::LogFormat::Json,
            "no variable set falls back to the JSON fleet default"
        );
    }

    #[test]
    fn otel_resource_attribute_parser_trims_and_drops_invalid_pairs() {
        let attrs = super::otel_resource_attribute_pairs(
            "service.version=1.2.3, missing-value=, =missing-key, cloud.region = us-east-1 ,bad,auth.token=do-not-export,database_password=nope",
        );

        assert_eq!(
            attrs,
            vec![
                ("service.version".to_string(), "1.2.3".to_string()),
                ("cloud.region".to_string(), "us-east-1".to_string()),
            ]
        );
    }

    #[test]
    fn otel_resource_attribute_parser_preserves_equals_inside_values() {
        let attrs = super::otel_resource_attribute_pairs(
            "build.url=https://ci.example/run?branch=main,service.version=1.2.3+sha=abc",
        );

        assert_eq!(
            attrs,
            vec![
                (
                    "build.url".to_string(),
                    "https://ci.example/run?branch=main".to_string(),
                ),
                ("service.version".to_string(), "1.2.3+sha=abc".to_string(),),
            ]
        );
    }

    #[test]
    fn sensitive_resource_attribute_keys_are_rejected() {
        for key in [
            "authorization",
            "http.cookie",
            "db.password",
            "service-api-key",
            "access_token",
            // Substring matching also covers infix/prefix placements that the
            // earlier exact/suffix rule let through.
            "my_secret_value",
            "token_id",
            "apitoken",
            "aws_credential_arn",
            "bearer_header",
            "tls_private_key_path",
            "session_cookie_name",
            "jwt_issuer",
            // Newly covered false-negatives: abbreviated/spelled-out passwords,
            // signing keys, and PII (email) reaching the exporter via a
            // resource-attribute key.
            "db_pwd",
            "user_passphrase",
            "webhook_signing_key",
            "user.email",
            "contact_email_address",
        ] {
            assert!(super::is_sensitive_attribute_key(key), "accepted {key}");
        }
        assert!(!super::is_sensitive_attribute_key("service.version"));
        assert!(!super::is_sensitive_attribute_key("cloud.region"));
        assert!(!super::is_sensitive_attribute_key("deployment.environment"));
        // Non-secret keys that must stay exported (guard against over-redaction).
        assert!(!super::is_sensitive_attribute_key("k8s.pod.name"));
        assert!(!super::is_sensitive_attribute_key("service.instance.id"));
    }
}

#[cfg(test)]
mod resource_assembly_tests {
    use std::collections::HashMap;

    use opentelemetry::{Key, KeyValue};

    /// Drive the assembly with a fake environment (no process-global env
    /// mutation, which races across parallel tests).
    fn attrs(env: &[(&str, &str)]) -> Vec<KeyValue> {
        let map: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        super::resource_attributes("test-service", |name| map.get(name).cloned())
    }

    fn value_of(attrs: &[KeyValue], key: &str) -> Option<String> {
        let key = Key::new(key.to_string());
        attrs
            .iter()
            .find(|kv| kv.key == key)
            .map(|kv| kv.value.to_string())
    }

    #[test]
    fn service_identity_defaults_are_always_present() {
        let attrs = attrs(&[]);
        assert_eq!(
            value_of(&attrs, "service.name").as_deref(),
            Some("test-service")
        );
        assert_eq!(
            value_of(&attrs, "service.namespace").as_deref(),
            Some("fiducia-cloud"),
            "namespace falls back to the fleet default"
        );
        assert_eq!(
            value_of(&attrs, "service.instance.id"),
            None,
            "no POD_NAME/HOSTNAME ⇒ no fabricated instance id"
        );
    }

    #[test]
    fn fiducia_env_wins_over_generic_and_empty_values_fall_through() {
        let attrs = attrs(&[
            ("FIDUCIA_DEPLOYMENT_ENV", "prod"),
            ("DEPLOYMENT_ENV", "staging"),
            ("FIDUCIA_CLUSTER", "   "), // whitespace-only must be skipped
        ]);
        assert_eq!(
            value_of(&attrs, "deployment.environment").as_deref(),
            Some("prod"),
            "the FIDUCIA_-prefixed variable takes precedence"
        );
        assert_eq!(
            value_of(&attrs, "fiducia.cluster"),
            None,
            "a whitespace-only value must not become an attribute"
        );
    }

    #[test]
    fn instance_id_prefers_pod_name_and_falls_back_to_hostname() {
        let with_pod = attrs(&[("POD_NAME", "brain-0"), ("HOSTNAME", "ignored")]);
        assert_eq!(
            value_of(&with_pod, "service.instance.id").as_deref(),
            Some("brain-0")
        );
        assert_eq!(
            value_of(&with_pod, "k8s.pod.name").as_deref(),
            Some("brain-0")
        );

        let with_host = attrs(&[("HOSTNAME", "dev-laptop")]);
        assert_eq!(
            value_of(&with_host, "service.instance.id").as_deref(),
            Some("dev-laptop"),
            "outside k8s the hostname identifies the replica"
        );
    }

    #[test]
    fn operator_resource_attributes_are_appended_with_secrets_redacted() {
        let attrs = attrs(&[(
            "OTEL_RESOURCE_ATTRIBUTES",
            "team=coordination,api_key=must-not-export",
        )]);
        assert_eq!(value_of(&attrs, "team").as_deref(), Some("coordination"));
        assert_eq!(
            value_of(&attrs, "api_key"),
            None,
            "sensitive operator-supplied keys are dropped"
        );
    }
}

#[cfg(test)]
mod init_smoke_tests {
    /// Child half of the subprocess test: only acts when the parent sets the
    /// env marker; a normal test run sees it pass as a no-op. Re-executing the
    /// test binary is the only way to observe `init()` end-to-end — it installs
    /// a process-global subscriber that can be set exactly once.
    #[test]
    fn subprocess_emit_helper() {
        if std::env::var("RUN_TELEMETRY_INIT_SUBPROCESS").as_deref() != Ok("1") {
            return;
        }
        let _telemetry = super::init("subproc-smoke");
        tracing::info!(probe = "init-smoke", "hello from the subprocess");
    }

    /// `init()` was previously untested end-to-end. Re-exec this test binary
    /// with the child marker: the stdout path (no OTLP endpoint) must install
    /// and emit parseable JSON lines carrying the service name and our probe
    /// event — the contract every fleet service and the otel-agent filelog
    /// pipeline rely on.
    #[test]
    fn init_emits_wellformed_json_lines_in_a_subprocess() {
        let exe = std::env::current_exe().expect("test binary path");
        let output = std::process::Command::new(exe)
            .args([
                "--exact",
                "init_smoke_tests::subprocess_emit_helper",
                "--nocapture",
            ])
            .env("RUN_TELEMETRY_INIT_SUBPROCESS", "1")
            .env("FIDUCIA_LOG_FORMAT", "json")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .env_remove("RUST_LOG")
            .output()
            .expect("spawn subprocess");
        assert!(
            output.status.success(),
            "subprocess failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let events: Vec<serde_json::Value> = stdout
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert!(!events.is_empty(), "no JSON log lines on stdout:\n{stdout}");

        let startup = events
            .iter()
            .find(|event| {
                event["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("telemetry initialized"))
            })
            .unwrap_or_else(|| panic!("startup line missing:\n{stdout}"));
        assert_eq!(
            startup["service.name"], "subproc-smoke",
            "the startup line must carry the service name: {startup}"
        );
        assert_eq!(startup["log.format"], "json");

        let probe = events
            .iter()
            .find(|event| event["probe"] == "init-smoke")
            .unwrap_or_else(|| panic!("probe event missing:\n{stdout}"));
        assert_eq!(probe["level"], "INFO");
        assert!(
            probe["target"].as_str().is_some(),
            "structured metadata (target) must be present: {probe}"
        );
    }

    /// FIDUCIA_LOG_FORMAT=text must switch init() to the human-readable
    /// layer: the startup line on stdout is NOT a JSON object (so the
    /// filelog/JSON pipeline contract does not silently apply) while still
    /// carrying the service name for a human reader.
    #[test]
    fn init_emits_human_text_lines_when_text_format_is_requested() {
        let exe = std::env::current_exe().expect("test binary path");
        let output = std::process::Command::new(exe)
            .args([
                "--exact",
                "init_smoke_tests::subprocess_emit_helper",
                "--nocapture",
            ])
            .env("RUN_TELEMETRY_INIT_SUBPROCESS", "1")
            .env("FIDUCIA_LOG_FORMAT", "text")
            // Keep the compact layer's output free of ANSI escapes so the
            // assertions below see plain text.
            .env("NO_COLOR", "1")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .env_remove("RUST_LOG")
            .output()
            .expect("spawn subprocess");
        assert!(
            output.status.success(),
            "subprocess failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let startup = stdout
            .lines()
            .find(|line| line.contains("telemetry initialized"))
            .unwrap_or_else(|| panic!("startup line missing:\n{stdout}"));
        assert!(
            serde_json::from_str::<serde_json::Value>(startup).is_err(),
            "text format must not emit a JSON startup line: {startup}"
        );
        assert!(
            startup.contains("subproc-smoke"),
            "the human-format startup line must still name the service: {startup}"
        );
        // The whole stream is human text: no line on stdout parses as a JSON
        // object, so a JSON log pipeline cannot half-apply.
        assert!(
            !stdout
                .lines()
                .any(|line| serde_json::from_str::<serde_json::Value>(line)
                    .is_ok_and(|value| value.is_object())),
            "text format must not emit any JSON object lines:\n{stdout}"
        );
    }
}
