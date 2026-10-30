# OpacusR signaling + relay server image.
# Build from the repository root:
#   docker build -f docker/server.Dockerfile -t opacusr/server:latest .

FROM rust:1-bookworm AS builder

WORKDIR /build

# Cache dependencies: manifest layer first, then sources.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release -p opacusr-server && \
    strip target/release/opacusr-server

FROM debian:bookworm-slim

LABEL org.opencontainers.image.title="OpacusR Server" \
      org.opencontainers.image.description="OpacusR signaling (WebSocket) + TCP relay server" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.source="https://github.com/opacusio/opacusr"

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 1000 --no-create-home opacusr

COPY --from=builder /build/target/release/opacusr-server /usr/local/bin/opacusr-server

USER opacusr

EXPOSE 8080 8081

# RUST_LOG: error | info (default) | debug | trace
ENV RUST_LOG=info

ENTRYPOINT ["/usr/local/bin/opacusr-server"]
CMD ["--host", "0.0.0.0", "--port", "8080", "--relay-port", "8081"]
