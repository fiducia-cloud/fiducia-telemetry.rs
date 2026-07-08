# syntax=docker/dockerfile:1
# CI/test image for the shared telemetry library.
FROM rust:1-slim-bookworm
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates
WORKDIR /build
ARG INTERFACES_REF=main
RUN git clone --depth 1 --branch "$INTERFACES_REF" \
    https://github.com/fiducia-cloud/fiducia-interfaces.git fiducia-interfaces
COPY . fiducia-telemetry.rs
WORKDIR /build/fiducia-telemetry.rs
RUN cargo test
# Run the container as an unprivileged user instead of root. Create the user and
# hand it the build tree + cargo caches so `cargo test` can still write target/
# and the registry lock when the image is run.
RUN useradd --create-home --uid 10001 ci \
    && chown -R ci:ci /build "${CARGO_HOME:-/usr/local/cargo}"
USER ci
CMD ["cargo", "test"]
