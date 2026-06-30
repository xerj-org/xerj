# Multi-arch container — v0.8 8-V6.
#
# Build with `docker buildx build --platform linux/amd64,linux/arm64 -t xerj/xerj:0.8.0 .`
#
# Build stage
FROM --platform=$BUILDPLATFORM rust:1.94-slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p xerj-server && strip target/release/xerj

# Runtime stage — distroless-like minimal
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/xerj /usr/local/bin/xerj
RUN mkdir -p /data
EXPOSE 8080 9200 8081
ENV XERJ_LOG=info

# Container HEALTHCHECK uses the v0.8 readiness probe — kubelet hits the
# same endpoint via Helm-chart probe configs.
HEALTHCHECK --interval=10s --timeout=2s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9200/health/ready || exit 1

ENTRYPOINT ["xerj"]
CMD ["--insecure", "--data-dir", "/data"]
