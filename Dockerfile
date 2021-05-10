FROM rust:1.52.0 as dependencies

WORKDIR /opt/dockyard
COPY Cargo.lock Cargo.toml build.rs ./
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs
RUN cargo build --release

FROM rust:1.52.0 as application
WORKDIR /opt/dockyard
COPY --from=dependencies /opt/dockyard/Cargo.toml /opt/dockyard/Cargo.lock /opt/dockyard/build.rs ./
COPY --from=dependencies /opt/dockyard/target target
COPY --from=dependencies /usr/local/cargo /usr/local/cargo
COPY src src
RUN cargo build --release

FROM debian:stable-slim
LABEL com.github.aig787.dockyard.command=true
LABEL com.github.aig787.dockyard.disabled=true
ENV OUTPUT_TYPE="directory"
ENV OUTPUT="/tmp"
ENV ARGS=""
COPY --from=application /opt/dockyard/target/release/dockyard /usr/local/bin/dockyard
CMD /usr/local/bin/dockyard watch --output-type ${OUTPUT_TYPE} ${OUTPUT} ${ARGS}