# syntax=docker/dockerfile:1
# CI/test image for the shared telemetry library.
FROM rust:1.95.0-slim-bookworm@sha256:d7482085ff5b415f84dba5647ae71606650bdef00db7aeb69f4b3d170c3e4082
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates
RUN useradd --create-home --uid 10001 ci \
    && install -d -o 10001 -g 10001 /build /home/ci/.cargo
ENV CARGO_HOME=/home/ci/.cargo
USER 10001:10001
WORKDIR /build
# Immutable cross-repository input. Bump this SHA together with the CI checkout.
ARG INTERFACES_SHA=bbd8b52ce729ec34b0a9bff4dda6d0a448181797
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
