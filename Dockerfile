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
CMD ["cargo", "test"]
