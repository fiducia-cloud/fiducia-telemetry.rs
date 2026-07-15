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
        Self::from_value(
            &std::env::var("FIDUCIA_LOG_FORMAT")
                .or_else(|_| std::env::var("OTEL_LOG_FORMAT"))
                // Compatibility for services that predate the shared telemetry
                // crate. Fleet-specific variables above keep precedence.
                .or_else(|_| std::env::var("LOG_FORMAT"))
                .unwrap_or_else(|_| "json".to_string()),
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
    let mut attrs = vec![
        KeyValue::new("service.name", service_name.to_string()),
        KeyValue::new(
            "service.namespace",
            env_or("OTEL_SERVICE_NAMESPACE", "fiducia-cloud"),
        ),
    ];

    push_env_attr(
        &mut attrs,
        &["FIDUCIA_DEPLOYMENT_ENV", "DEPLOYMENT_ENV"],
        "deployment.environment",
    );
    push_env_attr(&mut attrs, &["FIDUCIA_CLUSTER"], "fiducia.cluster");
    push_env_attr(&mut attrs, &["FIDUCIA_CLUSTER_ID"], "fiducia.cluster_id");
    push_env_attr(&mut attrs, &["FIDUCIA_CLOUD_PROVIDER"], "cloud.provider");
    push_env_attr(&mut attrs, &["FIDUCIA_CLOUD_REGION"], "cloud.region");
    push_env_attr(&mut attrs, &["POD_NAMESPACE"], "k8s.namespace.name");
    push_env_attr(&mut attrs, &["POD_NAME"], "k8s.pod.name");
    push_env_attr(&mut attrs, &["NODE_NAME"], "k8s.node.name");
    push_env_attr(&mut attrs, &["SERVICE_VERSION"], "service.version");
    // Distinguish replicas of the same service: pod name in k8s, hostname
    // elsewhere. Without this, multi-replica traces collapse into one instance.
    push_env_attr(&mut attrs, &["POD_NAME", "HOSTNAME"], "service.instance.id");
    push_otel_resource_attributes(&mut attrs);

    Resource::new(attrs)
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

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn push_env_attr(attrs: &mut Vec<KeyValue>, names: &[&str], key: &'static str) {
    for name in names {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                attrs.push(KeyValue::new(key, value));
                return;
            }
        }
    }
}

fn push_otel_resource_attributes(attrs: &mut Vec<KeyValue>) {
    let Ok(raw) = std::env::var("OTEL_RESOURCE_ATTRIBUTES") else {
        return;
    };

    for (key, value) in otel_resource_attribute_pairs(&raw) {
        attrs.push(KeyValue::new(key, value));
    }
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
