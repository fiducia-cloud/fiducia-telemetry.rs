# syntax=docker/dockerfile:1
# CI/test image for the shared telemetry library.
FROM rust:1.97.0-slim-bookworm@sha256:6d220bf85c74e842a79da63997af8d2e74455c0b8847d8bb3a5888572334991d
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates
RUN useradd --create-home --uid 10001 ci \
    && install -d -o 10001 -g 10001 /build /home/ci/.cargo
ENV CARGO_HOME=/home/ci/.cargo
USER 10001:10001
WORKDIR /build
# Immutable cross-repository input. Bump this SHA together with the CI checkout.
ARG INTERFACES_SHA=487e470c45ab5851e8f6f3b1dc048fe067fbf408
RUN git init fiducia-interfaces \
    && git -C fiducia-interfaces remote add origin \
       https://github.com/fiducia-cloud/fiducia-interfaces.git \
    && git -C fiducia-interfaces fetch --depth 1 origin "$INTERFACES_SHA" \
    && git -C fiducia-interfaces checkout --detach FETCH_HEAD \
    && test "$(git -C fiducia-interfaces rev-parse HEAD)" = "$INTERFACES_SHA"
COPY --chown=10001:10001 . fiducia-telemetry.rs
WORKDIR /build/fiducia-telemetry.rs
RUN cargo test --locked
CMD ["cargo", "test", "--locked"]
