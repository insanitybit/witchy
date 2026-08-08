#!/bin/sh
# Entrypoint for the PUBLIC coven origin (see ./Dockerfile and ./fly.toml).
#
# It runs TWO processes and supervises them:
#   1. `witchy coven-serve`  — the registry, bound to LOOPBACK 127.0.0.1:8787.
#   2. `witchy sandbox coven_web.witchy` — the glamour web UI + same-origin
#      reverse proxy, bound to 0.0.0.0:8080 (the only public listener), pointing
#      its upstream at the loopback registry.
# If EITHER process exits, the entrypoint tears down the other and exits nonzero
# so Fly restarts the whole machine — the origin never half-serves.
#
# The container starts as root ONLY for three chores that need it — materialize
# the signing seed onto tmpfs, chown the Fly volume (mounted root-owned), and set
# always-overcommit — then execs BOTH servers as the unprivileged `coven` user
# via setpriv (util-linux, present in debian-slim). The long-running servers are
# never root; only this tiny supervisor shell is.
set -eu

# Loopback registry + public web listener. WebAuthn ties the RP id/origin to the
# public host, so both come from the Fly app's public hostname.
COVEN_LOOPBACK="127.0.0.1:8787"
WEB_ADDR="0.0.0.0:8080"
WEB_URL="https://witchy.fly.dev"
RP_ID="witchy.fly.dev"
ROOT="${COVEN_ROOT:-/data}"
DIST="/app/dist"
APP="/app/coven_web.witchy"

if [ -z "${COVEN_SIGNING_SEED:-}" ]; then
    echo "entrypoint: COVEN_SIGNING_SEED is not set." >&2
    echo "  Set it as a Fly secret: fly secrets set COVEN_SIGNING_SEED=\$(openssl rand -hex 32)" >&2
    exit 1
fi
# witchy validates the format too (64 hex chars = a 32-byte seed), but failing
# here gives a clearer message than a post-boot crash loop.
if [ "${#COVEN_SIGNING_SEED}" -ne 64 ]; then
    echo "entrypoint: COVEN_SIGNING_SEED must be exactly 64 hex characters (openssl rand -hex 32), got ${#COVEN_SIGNING_SEED}" >&2
    exit 1
fi

# Seed file on tmpfs, readable only by the runtime user. Both processes read it:
# coven-serve as the registry root key, coven-web as its `signing` secret (session
# + OAuth-state MAC). `--signing-key <file>` is how both receive it.
SEED_DIR=/dev/shm
[ -d "$SEED_DIR" ] && [ -w "$SEED_DIR" ] || SEED_DIR=/tmp
SEED_FILE="$SEED_DIR/coven-signing.seed"
umask 077
printf '%s\n' "$COVEN_SIGNING_SEED" > "$SEED_FILE"
unset COVEN_SIGNING_SEED
chown coven:coven "$SEED_FILE"
chmod 0400 "$SEED_FILE"

# The registry store. A Fly volume mounted at /data arrives owned by root; hand
# it to the runtime user so the registry's Dir capability can write it.
mkdir -p "$ROOT"
chown coven:coven "$ROOT"

# The interpreter/compile path reserves a 4 GiB virtual (lazily committed) stack
# for its deep-recursion thread. On a small VM the default overcommit heuristic
# refuses that mapping and the process crash-loops with `pthread_create ...
# Resource temporarily unavailable` before binding. Always-overcommit makes the
# (never-committed) reservation admissible. Needs root — done before setpriv.
echo 1 > /proc/sys/vm/overcommit_memory || true

# setpriv preserves the environment; point HOME at the runtime user's home so
# witchy's embedded-wasm cache (~/.cache/witchy) is writable (best-effort — a
# miss only costs a recompile at boot).
HOME=/home/coven
export HOME

# ---- 1. coven-serve (registry) on loopback ----------------------------------
# Trust specs are optional: COVEN_TRUST_ISSUERS holds whitespace-separated
# `--trust-issuer` values, each `<issuer>=<pubkey-hex>` (or `<issuer>=jwks:<json>`).
# Without them the registry runs ANONYMOUS — never do that on a public URL.
set -- witchy coven-serve --addr "$COVEN_LOOPBACK" --root "$ROOT" --signing-key "$SEED_FILE"
for spec in ${COVEN_TRUST_ISSUERS:-}; do
    [ -n "$spec" ] && set -- "$@" --trust-issuer "$spec"
done
setpriv --reuid coven --regid coven --init-groups "$@" &
COVEN_PID=$!

# ---- 2. coven-web (public UI + same-origin proxy) ---------------------------
# `--dir` grants the served asset bundle; the two `--net` grants are the public
# listener and the loopback upstream. The positional args are
# <listen-addr> <upstream-addr> <public-origin> <webauthn-rp-id>.
setpriv --reuid coven --regid coven --init-groups \
    witchy sandbox --dir "$DIST" --net "$WEB_ADDR" --net "$COVEN_LOOPBACK" --signing-key "$SEED_FILE" \
    "$APP" "$WEB_ADDR" "$COVEN_LOOPBACK" "$WEB_URL" "$RP_ID" &
WEB_PID=$!

# ---- supervise: if EITHER exits, tear the other down and exit nonzero --------
# POSIX sh has no `wait -n`; poll both PIDs. The `kill -0` results are consumed by
# the while condition, so `set -e` does not fire on a live-process check.
while kill -0 "$COVEN_PID" 2>/dev/null && kill -0 "$WEB_PID" 2>/dev/null; do
    sleep 2
done
echo "entrypoint: a server process exited; shutting down the origin (Fly will restart)." >&2
kill "$COVEN_PID" "$WEB_PID" 2>/dev/null || true
wait "$COVEN_PID" 2>/dev/null || true
wait "$WEB_PID" 2>/dev/null || true
exit 1
