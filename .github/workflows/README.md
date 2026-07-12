# workflows

GitHub Actions pipelines for fiducia-telemetry:

- `ci.yml` — build, test, and lint (rustfmt/clippy) on push and pull request.

This repo is a shared library (no service image), so there are no
docker/deploy workflows.
