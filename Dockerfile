# Build stage
FROM rust:1.95-slim-bookworm AS builder

# Optional: set to your local crates.io mirror, e.g.:
#   tsinghua:  https://mirrors.tuna.tsinghua.edu.cn/git/crates.io-index.git
#   ustc:      https://mirrors.ustc.edu.cn/crates.io-index
#   sjtu:      https://mirrors.sjtug.sjtu.edu.cn/git/crates.io-index
ARG CARGO_REGISTRY=

# Configure cargo registry mirror if provided
RUN if [ -n "$CARGO_REGISTRY" ]; then \
        mkdir -p /usr/local/cargo && \
        printf '[registries.crates-io]\nprotocol = "sparse"\n\n[source.crates-io]\nreplace-with = "mirror"\n\n[source.mirror]\nregistry = "%s"\n' "$CARGO_REGISTRY" > /usr/local/cargo/config.toml; \
    fi

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tspan-tui/Cargo.toml ./tspan-tui/Cargo.toml
COPY tspan-tui/src ./tspan-tui/src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release -p tspan-server

# Runtime stage (minimal image)
FROM gcr.io/distroless/cc-debian12
WORKDIR /app
COPY --from=builder /app/target/release/tspan-server /app/tspan-server
COPY --from=builder /app/target/release/tspan-backup /app/tspan-backup
# data.db will be mounted via PVC to /app/data/
ENV DATABASE_URL=/app/data/data.db
ENV BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/app/tspan-server"]
