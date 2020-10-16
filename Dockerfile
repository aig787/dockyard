# syntax=docker/dockerfile:experimental
FROM rust:1.47.0 as builder

WORKDIR /opt/dockyard

COPY . .

RUN --mount=type=cache,target=target \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release

# Copy binaries into normal layers
RUN --mount=type=cache,target=target \
    cp ./target/release/dockyard /usr/local/bin/dockyard

FROM debian:stable-slim
COPY --from=builder /usr/local/bin/dockyard /usr/local/bin/dockyard
CMD ["/usr/local/bin/dockyard", "--help"]