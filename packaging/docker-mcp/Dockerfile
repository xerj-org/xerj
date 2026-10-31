# syntax=docker/dockerfile:1
# `xerj mcp` as a stdio MCP server in a container, for the Docker MCP
# Catalog (github.com/docker/mcp-registry) and any client that launches
# MCP servers with `docker run -i`.
#
# This image does not build from source; it fetches the published musl
# release binary and checks it against the release's .sha256 asset. The
# MCP server proxies to a XERJ node, so point XERJ_URL at one you run on
# the host (default below) or elsewhere. The Corpus Hub tools work with
# no node at all.
#
#   docker build -f packaging/docker-mcp/Dockerfile -t xerj-mcp .
#   docker run -i --rm -e XERJ_URL=http://host.docker.internal:9200 xerj-mcp
#
# The server image for the engine itself is engine/Dockerfile.

FROM alpine:3.20 AS fetch
ARG XERJ_VERSION=1.0.0-rc.100
ARG TARGETARCH
RUN apk add --no-cache curl ca-certificates
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) triple=x86_64-unknown-linux-musl ;; \
      arm64) triple=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported arch ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    name="xerj-${XERJ_VERSION}-${triple}.tar.gz"; \
    base="https://github.com/xerj-org/xerj/releases/download/v${XERJ_VERSION}"; \
    cd /tmp; \
    curl -fsSLO "${base}/${name}"; \
    curl -fsSLO "${base}/${name}.sha256"; \
    echo "$(cut -d' ' -f1 "${name}.sha256")  ${name}" | sha256sum -c -; \
    tar xzf "${name}"; \
    install -m 0755 "xerj-${XERJ_VERSION}-${triple}/xerj" /usr/local/bin/xerj

FROM alpine:3.20
LABEL io.modelcontextprotocol.server.name="io.github.xerj-org/xerj" \
      org.opencontainers.image.source="https://github.com/xerj-org/xerj" \
      org.opencontainers.image.description="XERJ: the search engine for AI agents. MCP server (stdio)." \
      org.opencontainers.image.licenses="Apache-2.0"
RUN apk add --no-cache ca-certificates && adduser -D -u 10001 xerj
COPY --from=fetch /usr/local/bin/xerj /usr/local/bin/xerj
USER xerj
ENV XERJ_URL=http://host.docker.internal:9200
ENTRYPOINT ["/usr/local/bin/xerj", "mcp"]
