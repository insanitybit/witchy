# Container image for the PUBLIC coven registry origin: the coven-web glamour UI
# (projects/coven-web/src/coven_web.witchy) fronting an embedded coven registry
# (`witchy coven-serve`) on loopback. coven-web serves the styled SPA + the full
# security-header stack AND same-origin-proxies both `/api/coven/*` (for the SPA)
# and the bare `/coven/*` read routes (for `witchy pm` clients pointed at this
# origin) to the loopback registry.
#
# Build from the REPO ROOT (the builder compiles the whole workspace; coven + std
# sources are include_str!-embedded into the binary, and the coven-web web bundle
# must be built first with projects/coven-web/web/build.sh):
#
#   docker build -f projects/coven-web/deploy/Dockerfile -t witchy-coven-web .
#
# Fly config: ./fly.toml. This mirrors projects/coven/deploy/Dockerfile (same
# from-source, glibc-matched builder/runtime pairing) so the committed image is
# reproducible from a fresh checkout with no external binary blob — rather than
# copying a prebuilt release, which would leave the Dockerfile referencing an
# artifact that is not in the repo.

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
# debian:trixie-slim matches the builder's glibc (same trixie release), so the
# binary the builder linked is the binary this stage runs. It also ships setpriv
# (util-linux) which the entrypoint uses to drop privileges after its root-only
# chores (seed file, volume chown, the overcommit sysctl).
FROM debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258

# ca-certificates: outbound TLS trust roots (social-login JWKS/token fetches).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /home/coven --shell /usr/sbin/nologin coven \
    && mkdir -p /data \
    && chown coven:coven /data

COPY --from=builder /build/target/release/witchy /usr/local/bin/witchy
# coven-web runs from SOURCE via `witchy sandbox`; the file imports only std
# modules (embedded in the binary), so the single source file + the built web
# bundle are all the image needs. coven-serve runs embedded from the binary.
COPY projects/coven-web/src/coven_web.witchy /app/coven_web.witchy
COPY projects/coven-web/web/dist /app/dist
COPY projects/coven-web/deploy/entrypoint.sh /usr/local/bin/coven-web-entrypoint
RUN chmod 0755 /usr/local/bin/coven-web-entrypoint

# The registry store lives on a mounted volume at /data (fly.toml [mounts]).
VOLUME /data
EXPOSE 8080

# Liveness probe against a real route: GET /coven/rootpub is proxied by coven-web
# to the loopback registry and returns 200 with the registry's signing public key.
# No curl in the image - bash's /dev/tcp is enough for a loopback HTTP GET.
# Fly ignores Docker HEALTHCHECK; fly.toml carries the equivalent probe.
HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /coven/rootpub HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && head -1 <&3 | grep -q ' 200 '"]

# Starts as root for the entrypoint's root-only chores (seed file, volume chown,
# overcommit sysctl); both server processes run as `coven` via setpriv.
ENTRYPOINT ["/usr/local/bin/coven-web-entrypoint"]
