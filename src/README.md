# src — fiducia-telemetry

The Rust source for the shared telemetry library used by every fiducia.cloud
service. `lib.rs` exposes a single `init(service_name)` entry point that wires up
`tracing`/OpenTelemetry for the whole process (structured JSON logs, optional
OTLP export), so services get consistent, correlated telemetry with no per-repo
boilerplate. Credential-bearing OTLP endpoints are never logged, and sensitive
resource-attribute keys are dropped before export.
