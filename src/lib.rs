//! Shared telemetry setup for every fiducia.cloud Rust service.
//!
//! One call — [`init`] — wires up `tracing` for the whole process:
//!   * always: JSON structured logs to stdout with `RUST_LOG`/`EnvFilter`
//!     filtering (`FIDUCIA_LOG_FORMAT=text` for local human-readable logs);
//!   * when `OTEL_EXPORTER_OTLP_ENDPOINT` is set: an OpenTelemetry **OTLP** trace
//!     exporter (gRPC) so spans flow to a collector / Tempo, tagged with
//!     service and deployment resource attributes.
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
//!     fiducia_telemetry::init("fiducia-node");
//!     // ...
//! }
//! ```

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    runtime,
    trace::{Tracer, TracerProvider},
    Resource,
};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Initialize tracing + (optional) OTLP export for `service_name`.
///
/// Idempotent-ish: call exactly once per process, early in `main`.
pub fn init(service_name: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let log_format = LogFormat::from_env();

    match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(endpoint) if !endpoint.is_empty() => {
            // A misconfigured endpoint must NOT crash the service: on any
            // exporter build failure, fall back to stdout logging and carry on.
            match opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&endpoint)
                // A wedged collector must stall exports, not the process; the
                // batch worker drops batches that exceed this deadline.
                .with_timeout(std::time::Duration::from_secs(10))
                .build()
            {
                Ok(exporter) => {
                    let provider = TracerProvider::builder()
                        .with_batch_exporter(exporter, runtime::Tokio)
                        .with_resource(resource(service_name))
                        .build();
                    let tracer = provider.tracer("fiducia");
                    opentelemetry::global::set_tracer_provider(provider);
                    install_otlp_subscriber(filter, log_format, tracer);
                    tracing::info!(
                        service.name = service_name,
                        log.format = log_format.as_str(),
                        "telemetry: OTLP export enabled"
                    );
                }
                Err(_) => {
                    install_stdout_subscriber(filter, log_format);
                    // Exporter errors can echo a credential-bearing endpoint;
                    // keep the startup failure useful without logging its text.
                    tracing::error!("telemetry: OTLP exporter init failed; using stdout only");
                }
            }
        }
        _ => {
            install_stdout_subscriber(filter, log_format);
            tracing::info!(
                service.name = service_name,
                log.format = log_format.as_str(),
                "telemetry: stdout export enabled"
            );
        }
    }
}

/// Flush + shut down the tracer provider. Call before exit if you want a clean
/// final flush (long-running servers can skip it — batches flush periodically).
///
/// The underlying flush blocks; running it on a dedicated thread keeps this
/// safe to call from any context, including a current-thread Tokio runtime
/// (where blocking in-place would deadlock the batch worker).
pub fn shutdown() {
    let done = std::thread::spawn(|| {
        opentelemetry::global::shutdown_tracer_provider();
    })
    .join();
    if done.is_err() {
        eprintln!("telemetry: shutdown flush panicked; spans may be dropped");
    }
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
    Resource::new(resource_attributes(service_name, |name| {
        std::env::var(name).ok()
    }))
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

    fn value_of<'a>(attrs: &'a [KeyValue], key: &str) -> Option<String> {
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
        super::init("subproc-smoke");
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
                    .is_some_and(|message| message.contains("stdout export enabled"))
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
            .find(|line| line.contains("stdout export enabled"))
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
