# The Anyfetch MCP server as a container image, built from the release binary:
#   docker run -i --rm ghcr.io/aliasmode/anyfetch
# This stage runs on the build machine, so an image for another architecture needs no emulation.
FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS download
ARG VERSION
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl xz-utils
RUN arch=$([ "$TARGETARCH" = arm64 ] && echo aarch64 || echo x86_64) \
    && curl -fsSL "https://github.com/aliasmode/anyfetch/releases/download/v${VERSION}/anyfetch-${arch}-unknown-linux-gnu.tar.xz" \
    | tar -xJ --strip-components=1 -C /usr/local/bin "anyfetch-${arch}-unknown-linux-gnu/anyfetch"

FROM gcr.io/distroless/cc-debian12
COPY --from=download /usr/local/bin/anyfetch /usr/local/bin/anyfetch
LABEL io.modelcontextprotocol.server.name="io.github.aliasmode/anyfetch"
LABEL org.opencontainers.image.source="https://github.com/aliasmode/anyfetch"
LABEL org.opencontainers.image.description="Web search and web fetch for AI agents, with no API keys"
LABEL org.opencontainers.image.licenses="Apache-2.0"
ENTRYPOINT ["/usr/local/bin/anyfetch"]
CMD ["mcp"]
