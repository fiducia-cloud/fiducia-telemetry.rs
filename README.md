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
  terminal-friendly logs. `OTEL_LOG_FORMAT` and legacy `LOG_FORMAT` are
  fallbacks, in that order.
- **When `OTEL_EXPORTER_OTLP_ENDPOINT` is set:** an OpenTelemetry **OTLP** (gRPC)
  trace exporter, with service/deployment/Kubernetes resource attributes. Spans
  flow to the local collector first.

The production path is collector-first: services emit JSON stdout logs for node
collection and send OTLP traces to a local OpenTelemetry collector. With no
endpoint set (local dev), telemetry degrades to stdout-only logging.

## Config

All configuration is via environment variables. Treat the OTLP endpoint as
sensitive because deployments sometimes embed credentials in URL userinfo or
query parameters; it is never logged or exposed as a CLI flag.

| Var | Required | Secret | Description |
|-----|----------|--------|-------------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | no | potentially | OTLP gRPC endpoint, e.g. `http://fiducia-otel-agent:4317`; enables trace export when set. Unset = stdout-only. Never logged or accepted on argv. |
| `FIDUCIA_LOG_FORMAT` | no | no | Log output format: `json` (default) or `text` / `plain` / `pretty` / `compact`. |
| `OTEL_LOG_FORMAT` | no | no | Fallback log format used when `FIDUCIA_LOG_FORMAT` is unset. |
| `LOG_FORMAT` | no | no | Legacy fallback used only when both telemetry-specific log-format variables are unset. |
| `NO_COLOR` | no | no | If set (any value), disables ANSI color in text logs. |
| `OTEL_RESOURCE_ATTRIBUTES` | no | must not contain secrets | Comma-separated extra resource attributes (`key=value,...`). Sensitive key names such as token/password/cookie/API key are dropped. |
| `OTEL_SERVICE_NAMESPACE` | no | no | Service namespace resource attribute, default `fiducia-cloud`. |
| `FIDUCIA_DEPLOYMENT_ENV` | no | no | Deployment environment resource attribute. |
| `FIDUCIA_CLUSTER` / `FIDUCIA_CLUSTER_ID` | no | no | Cluster resource attributes from the k8s topology ConfigMap. |
| `POD_NAMESPACE` / `POD_NAME` / `NODE_NAME` | no | no | Kubernetes resource attributes from the downward API. |
| `RUST_LOG` | no | no | Log/trace filter, e.g. `info,fiducia_node=debug`. |

### Setting config from CLI flags (flags-2-env)

The `FIDUCIA_LOG_FORMAT`, `NO_COLOR`, and `OTEL_LOG_FORMAT` vars can be driven
from CLI flags via the pinned `ORESoftware/flags-2-env` parser (schema in
`.cli-flags.toml`, audited in CI by `.github/workflows/cli-flags.yml`):

```bash
git submodule update --init --recursive
make -C vendor/flags-2-env all
OTEL_EXPORTER_OTLP_ENDPOINT=http://fiducia-otel-agent:4317 \
  scripts/with-flags2env.sh --log-format=text --no-color -- cargo run --locked
```

`scripts/with-flags2env.sh` maps the flags to the env vars `init()` reads, then
execs the given command.

## Reproducible CI/test image

This crate consumes generated contracts from the sibling `fiducia-interfaces`
repository. CI and the test Dockerfile pin it to commit
`487e470c45ab5851e8f6f3b1dc048fe067fbf408` instead of a moving branch. The
Docker build checks that commit out detached and verifies that the resulting
full `HEAD` equals `INTERFACES_SHA`; a branch, tag, or abbreviated hash fails
closed. Both the image build and its default test command require the committed
Cargo lockfile. After installing system packages, the Dockerfile switches to
numeric uid/gid `10001` before fetching contracts, compiling, or running tests;
the build tree and Cargo home are owned only by that unprivileged account.

```bash
docker build \
  --build-arg INTERFACES_SHA=<40-character-commit-sha> \
  -t fiducia-telemetry:test .
```

## Security / hardening

`cargo audit` is **clean** (no known advisories in the dependency tree, 127
crates scanned). Endpoint values and exporter error text are not logged, and
sensitive resource-attribute keys are rejected before export. Dependency bumps
are kept within semver to avoid breaking the shared `init()` contract.

## Roadmap

Traces and JSON logs ship today. Next, behind the same `init`: explicit app
metrics — the deployed otel-agent Collector (`fiducia-infra/base/observability`)
already runs a live OTLP metrics pipeline to the observability gateway, but no
service emits OTLP metrics yet (today's metrics travel via the node-sidecar's
Prometheus `/metrics` and lambda's hand-rolled endpoint). Also: high-value
structured events the gateway can store without ingesting every raw log line.

## Used as a dependency

Pinned **git** dependency (so a telemetry change is a deliberate version bump):

```toml
fiducia-telemetry = { git = "https://github.com/fiducia-cloud/fiducia-telemetry.rs", tag = "v0.1.0" }
```

## Consumers

`fiducia-node` · `fiducia-brain` · `fiducia-load-balance` · `fiducia-node-sidecar`
· `fiducia-auth` · `fiducia-backend` · `fiducia-admin` · `fiducia-customer`
· `fiducia-ai-agent-manager` · `fiducia-lambda-service` · `fiducia-ai-agent-bridge`
· `fiducia-ai-agent-control-plane` · `fiducia-operations-control-plane` · `fiducia-memory`
