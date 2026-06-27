//! Shared telemetry setup for every fiducia.cloud Rust service.
//!
//! One call — [`init`] — wires up `tracing` for the whole process:
//!   * always: a `fmt` layer to stdout with `RUST_LOG`/`EnvFilter` filtering;
//!   * when `OTEL_EXPORTER_OTLP_ENDPOINT` is set: an OpenTelemetry **OTLP** trace
//!     exporter (gRPC) so spans flow to a collector / Tempo, tagged with
//!     `service.name`.
//!
//! Direct-OTLP, all-Rust: services export OTLP straight to the backends; there's
//! no Go collector in the path unless you choose to add one. With no endpoint
//! configured (local dev), it degrades to plain stdout logging.
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
use opentelemetry_sdk::{runtime, trace::TracerProvider, Resource};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

/// Initialize tracing + (optional) OTLP export for `service_name`.
///
/// Idempotent-ish: call exactly once per process, early in `main`.
pub fn init(service_name: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer();

    match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(endpoint) if !endpoint.is_empty() => {
            // A misconfigured endpoint must NOT crash the service: on any
            // exporter build failure, fall back to stdout logging and carry on.
            match opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()
            {
                Ok(exporter) => {
                    let provider = TracerProvider::builder()
                        .with_batch_exporter(exporter, runtime::Tokio)
                        .with_resource(Resource::new(vec![KeyValue::new(
                            "service.name",
                            service_name.to_string(),
                        )]))
                        .build();
                    let tracer = provider.tracer("fiducia");
                    opentelemetry::global::set_tracer_provider(provider);
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(fmt_layer)
                        .with(tracing_opentelemetry::layer().with_tracer(tracer))
                        .init();
                    tracing::info!(service = service_name, "telemetry: OTLP export enabled");
                }
                Err(e) => {
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(fmt_layer)
                        .init();
                    tracing::error!(
                        "telemetry: OTLP exporter init failed ({e}); using stdout only"
                    );
                }
            }
        }
        _ => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .init();
        }
    }
}

/// Flush + shut down the tracer provider. Call before exit if you want a clean
/// final flush (long-running servers can skip it — batches flush periodically).
pub fn shutdown() {
    opentelemetry::global::shutdown_tracer_provider();
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
}
