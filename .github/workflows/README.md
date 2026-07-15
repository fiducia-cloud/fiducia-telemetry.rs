# workflows

GitHub Actions pipelines for fiducia-telemetry:

- `ci.yml` — enforce formatting, locked all-target Clippy/tests, and pinned
  cargo-audit on push and pull request.
  It checks out `fiducia-interfaces` at the exact commit also pinned by the test
  Dockerfile, and dependency-resolving Cargo commands require `--locked`.

This repo is a shared library (no service image), so there are no
docker/deploy workflows.

## Security baseline

Every executable workflow uses explicit least-privilege permissions, immutable
third-party action or container references, non-persisted checkout credentials,
concurrency control, and a job timeout. The main CI workflow validates this
directory with the digest-pinned actionlint container. Environment mutation is
forbidden unless this README documents a repository-specific platform exception.
