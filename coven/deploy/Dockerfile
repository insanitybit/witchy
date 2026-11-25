# Container image for the coven registry (`witchy coven-serve`), the embedded
# witchy-language registry server bundled into the toolchain binary.
# Build from the REPO ROOT (the builder compiles the whole workspace, and the
# coven + std sources are include_str!-embedded into the binary):
#
#   docker build -f projects/coven/deploy/Dockerfile -t witchy-coven .
#
# Deployment runbook: projects/coven/DEPLOY.md. Fly config: ./fly.toml.

# --- builder -----------------------------------------------------------------
# Pinned by digest (the tag is documentation, the digest is the pin).
# rust:1.95-slim-trixie matches the workspace's rust-version = "1.95".
FROM rust:1.95-slim-trixie@sha256:e14e87345b4d5964ddcc3491d27ee046a0f23820f340c3c1e24da6880141f7c0 AS builder

WORKDIR /build
COPY . .
# Only the toolchain binary; --locked so the image can never build against
# drifted dependencies.
RUN cargo build --release --locked --bin witchy

# --- runtime -----------------------------------------------------------------
# debian-slim rather than distroless: the entrypoint is a shell script (it has
# root-only chores - materializing the Fly secret into a tmpfs seed file and
# chowning the volume mount - before dropping privileges via setpriv, which
# ships in debian's essential util-linux). A distroless-with-shell image would
# save little (~50 MB) and lose setpriv + the glibc/apt security-update cadence
# the rust builder image itself tracks. Same trixie release as the builder, so
# the glibc the binary linked against is the glibc it runs on.
FROM debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258

# ca-certificates: outbound TLS trust roots. Unused by the M1 server (pinned
# key / inline JWKS only), needed the moment live JWKS-over-HTTPS discovery
# (RFC-0116 track 2) lands; tiny, so bake it now rather than rebuild then.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /home/coven --shell /usr/sbin/nologin coven \
    && mkdir -p /data \
    && chown coven:coven /data

COPY --from=builder /build/target/release/witchy /usr/local/bin/witchy
COPY projects/coven/deploy/entrypoint.sh /usr/local/bin/coven-entrypoint
RUN chmod 0755 /usr/local/bin/coven-entrypoint

# The registry store lives on a mounted volume at /data (fly.toml [mounts]).
VOLUME /data
EXPOSE 8080

# Liveness probe against a real route: GET /coven/rootpub returns 200 with the
# registry's signing public key (projects/coven/src/coven.witchy route table).
# No curl in the image - bash's /dev/tcp is enough for a loopback HTTP GET.
# Note Fly ignores Docker HEALTHCHECK; fly.toml carries the equivalent
# [[http_service.checks]] probe. This one serves plain `docker run` users.
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /coven/rootpub HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && head -1 <&3 | grep -q ' 200 '"]

# Starts as root for the entrypoint's chown/seed-file chores; the server itself
# runs as `coven` (setpriv drop inside the entrypoint - see its header comment).
ENTRYPOINT ["/usr/local/bin/coven-entrypoint"]
