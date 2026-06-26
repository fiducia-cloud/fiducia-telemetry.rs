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

- **Always:** a `tracing` `fmt` layer to stdout, filtered by `RUST_LOG` /
  `EnvFilter` (default `info`).
- **When `OTEL_EXPORTER_OTLP_ENDPOINT` is set:** an OpenTelemetry **OTLP** (gRPC)
  trace exporter, with `service.name` set to the name you pass. Spans flow
  straight to your backend (Tempo / a collector).

**Direct-OTLP, all-Rust:** services export OTLP themselves — no Go collector is
required in the path (add one only if you want central batching/routing). With no
endpoint set (local dev), it degrades to plain stdout logging.

## Config

| Env | Effect |
|-----|--------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | e.g. `http://otel-collector:4317` — enables OTLP trace export |
| `RUST_LOG` | log/trace filter (e.g. `info,fiducia_node=debug`) |

## Roadmap

Traces ship today. Next, behind the same `init`: OTLP **metrics** (Prometheus via
OTLP receiver / remote-write) and OTLP **logs** (Loki via its OTLP endpoint) — so
the Prometheus/Loki/Tempo trio is fed entirely over OTLP from Rust.

## Used as a dependency

Pinned **git** dependency (so a telemetry change is a deliberate version bump):

```toml
fiducia-telemetry = { git = "https://github.com/fiducia-cloud/fiducia-telemetry.rs", tag = "v0.1.0" }
```

## Consumers

`fiducia-node` · `fiducia-brain` · `fiducia-load-balance` · `fiducia-node-sidecar` · `fiducia-auth` · `fiducia-backend`
