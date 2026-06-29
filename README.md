# fiducia-telemetry

Shared **OpenTelemetry + tracing** setup for every fiducia.cloud Rust service.
One call wires up the whole process, so all services emit consistent, correlated
telemetry without per-repo boilerplate.

```rust
#[tokio::main]
async fn main() {
    fiducia_telemetry::init("fiducia-node");   // <- the only line each service needs
    // ...
}
```

## What `init` does

- **Always:** JSON structured logs to stdout, filtered by `RUST_LOG` /
  `EnvFilter` (default `info`). Set `FIDUCIA_LOG_FORMAT=text` for local
  terminal-friendly logs.
- **When `OTEL_EXPORTER_OTLP_ENDPOINT` is set:** an OpenTelemetry **OTLP** (gRPC)
  trace exporter, with service/deployment/Kubernetes resource attributes. Spans
  flow to the local collector first.

The production path is collector-first: services emit JSON stdout logs for node
collection and send OTLP traces to a local OpenTelemetry collector. With no
endpoint set (local dev), telemetry degrades to stdout-only logging.

## Config

| Env | Effect |
|-----|--------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | e.g. `http://fiducia-otel-agent:4317` — enables OTLP trace export |
| `FIDUCIA_LOG_FORMAT` | `json` by default; set `text` / `pretty` / `compact` for local logs |
| `OTEL_SERVICE_NAMESPACE` | service namespace resource attribute, default `fiducia-cloud` |
| `FIDUCIA_DEPLOYMENT_ENV` | deployment environment resource attribute |
| `FIDUCIA_CLUSTER` / `FIDUCIA_CLUSTER_ID` | cluster resource attributes from the k8s topology ConfigMap |
| `POD_NAMESPACE` / `POD_NAME` / `NODE_NAME` | Kubernetes resource attributes from downward API |
| `OTEL_RESOURCE_ATTRIBUTES` | comma-separated extra resource attributes |
| `RUST_LOG` | log/trace filter (e.g. `info,fiducia_node=debug`) |

## Roadmap

Traces and JSON logs ship today. Next, behind the same `init`: explicit app
metrics and high-value structured events that the observability gateway can
store in Cockroach TTL tables without ingesting every raw log line into SQL.

## Used as a dependency

Pinned **git** dependency (so a telemetry change is a deliberate version bump):

```toml
fiducia-telemetry = { git = "https://github.com/fiducia-cloud/fiducia-telemetry.rs", tag = "v0.1.0" }
```

## Consumers

`fiducia-node` · `fiducia-brain` · `fiducia-load-balance` · `fiducia-node-sidecar` · `fiducia-auth` · `fiducia-backend`
