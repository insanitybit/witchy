#!/bin/sh
# Entrypoint for the coven registry container (see ./Dockerfile and
# ../DEPLOY.md). It materializes the signing seed from the COVEN_SIGNING_SEED
# environment variable (a Fly secret) into a root-only tmpfs file, fixes the
# ownership of the mounted data volume, then drops privileges and execs
# `witchy coven-serve`.
#
# Why an env var -> file shuffle: `witchy coven-serve --signing-key` takes a
# FILE path holding 64 hex chars (src/main.rs load_signing_seed), while Fly
# secrets reach the machine only as environment variables. The seed is written
# under /dev/shm (tmpfs - never touches the disk or the volume), mode 0400,
# owned by the unprivileged runtime user, and the variable is unset before the
# server starts so the process environment no longer carries it.
#
# The container starts as root ONLY for two chores that need it - chown the
# Fly volume mount (volumes arrive root-owned) and write the seed file - then
# execs the server as the `coven` user via setpriv (util-linux, present in
# debian-slim). The long-running server process is never root.
set -eu

ADDR="${COVEN_ADDR:-0.0.0.0:8080}"
ROOT="${COVEN_ROOT:-/data}"

if [ -z "${COVEN_SIGNING_SEED:-}" ]; then
    echo "entrypoint: COVEN_SIGNING_SEED is not set." >&2
    echo "  Set it as a Fly secret: fly secrets set COVEN_SIGNING_SEED=\$(openssl rand -hex 32)" >&2
    exit 1
fi
# witchy validates the format too (64 hex chars = a 32-byte Ed25519 seed), but
# failing here gives a clearer message than a post-boot crash loop.
if [ "${#COVEN_SIGNING_SEED}" -ne 64 ]; then
    echo "entrypoint: COVEN_SIGNING_SEED must be exactly 64 hex characters (openssl rand -hex 32), got ${#COVEN_SIGNING_SEED}" >&2
    exit 1
fi

# Seed file on tmpfs, readable only by the runtime user.
SEED_DIR=/dev/shm
[ -d "$SEED_DIR" ] && [ -w "$SEED_DIR" ] || SEED_DIR=/tmp
SEED_FILE="$SEED_DIR/coven-signing.seed"
umask 077
printf '%s\n' "$COVEN_SIGNING_SEED" > "$SEED_FILE"
unset COVEN_SIGNING_SEED
chown coven:coven "$SEED_FILE"
chmod 0400 "$SEED_FILE"

# The registry store. A Fly volume mounted at /data arrives owned by root;
# hand it to the runtime user so the server's Dir capability can write it.
mkdir -p "$ROOT"
chown coven:coven "$ROOT"

# Assemble the server command. Trust specs are optional: COVEN_TRUST_ISSUERS
# holds newline-separated `--trust-issuer` values, each either
# `<issuer>=<ed25519-pubkey-hex>` or `<issuer>=jwks:<compact JWKS JSON>`
# (passed verbatim; see DEPLOY.md "Configuring issuer trust").
set -- witchy coven-serve --addr "$ADDR" --root "$ROOT" --signing-key "$SEED_FILE"
if [ -n "${COVEN_TRUST_ISSUERS:-}" ]; then
    while IFS= read -r spec; do
        [ -n "$spec" ] && set -- "$@" --trust-issuer "$spec"
    done <<EOF
$COVEN_TRUST_ISSUERS
EOF
fi

# setpriv preserves the environment; point HOME at the runtime user's home so
# witchy's embedded-wasm cache (~/.cache/witchy) is writable (it is best-effort
# - a miss only costs a recompile at boot - but there is no reason to miss).
HOME=/home/coven
export HOME
exec setpriv --reuid coven --regid coven --init-groups "$@"
