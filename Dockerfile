FROM rust:1.47.0 as dependencies

WORKDIR /opt/dockyard
COPY Cargo.lock Cargo.toml build.rs ./
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs
RUN cargo build --release

FROM rust:1.47.0 as application
WORKDIR /opt/dockyard
COPY --from=dependencies /opt/dockyard/Cargo.toml /opt/dockyard/Cargo.lock /opt/dockyard/build.rs ./
COPY --from=dependencies /opt/dockyard/target target
COPY --from=dependencies /usr/local/cargo /usr/local/cargo
COPY src src
RUN cargo build --release

FROM debian:stable-slim
COPY --from=application /opt/dockyard/target/release/dockyard /usr/local/bin/dockyard
CMD ["/usr/local/bin/dockyard", "--help"]