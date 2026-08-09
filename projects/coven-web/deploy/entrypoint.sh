#!/bin/sh
# Entrypoint for the PUBLIC coven origin (see ./Dockerfile and ./fly.toml).
#
# It runs TWO supervised processes:
#   1. `witchy coven-serve`  — the registry, bound to LOOPBACK 127.0.0.1:8787.
#   2. `witchy sandbox coven_web.witchy` — the glamour web UI + same-origin
#      reverse proxy, bound to 0.0.0.0:8080 (the only public listener).
# If EITHER exits, the entrypoint tears the other down and exits nonzero so Fly
# restarts the whole machine — the origin never half-serves.
#
# The container starts as root ONLY for its chores — materialize seed files onto
# tmpfs, chown the Fly volume, refresh the served asset bundle, set always-
# overcommit — then execs BOTH servers as the unprivileged `coven` user via
# setpriv. The long-running servers are never root.
set -eu

COVEN_LOOPBACK="127.0.0.1:8787"
WEB_ADDR="0.0.0.0:8080"
ROOT="${COVEN_ROOT:-/data}"
DIST="/app/dist"
APP="/app/coven_web.witchy"

# The public origin browsers reach — coven-web verifies every WebAuthn assertion
# against it AND mints its human-2FA identity tokens with `iss` = this origin
# (RFC-0119), so it must be EXACTLY the address-bar URL. The Fly app name is
# authoritative; override with COVEN_WEB_ORIGIN (a custom domain).
if [ -n "${COVEN_WEB_ORIGIN:-}" ]; then
    ORIGIN="$COVEN_WEB_ORIGIN"
elif [ -n "${FLY_APP_NAME:-}" ]; then
    ORIGIN="https://${FLY_APP_NAME}.fly.dev"
else
    ORIGIN="http://localhost:8080"
fi
if [ -n "${COVEN_WEB_RP_ID:-}" ]; then
    RP_ID="$COVEN_WEB_RP_ID"
else
    RP_ID="${ORIGIN#*://}"; RP_ID="${RP_ID%%/*}"; RP_ID="${RP_ID%%:*}"
fi

if [ -z "${COVEN_SIGNING_SEED:-}" ]; then
    echo "entrypoint: COVEN_SIGNING_SEED is not set (fly secrets set COVEN_SIGNING_SEED=\$(openssl rand -hex 32))." >&2
    exit 1
fi
if [ "${#COVEN_SIGNING_SEED}" -ne 64 ]; then
    echo "entrypoint: COVEN_SIGNING_SEED must be exactly 64 hex characters, got ${#COVEN_SIGNING_SEED}" >&2
    exit 1
fi

SEED_DIR=/dev/shm
[ -d "$SEED_DIR" ] && [ -w "$SEED_DIR" ] || SEED_DIR=/tmp
umask 077

# coven-serve's registry root key.
SEED_FILE="$SEED_DIR/coven-signing.seed"
printf '%s\n' "$COVEN_SIGNING_SEED" > "$SEED_FILE"
unset COVEN_SIGNING_SEED
chown coven:coven "$SEED_FILE"; chmod 0400 "$SEED_FILE"

# coven-web's signing key — a DISTINCT trust domain from the registry root key.
# It backs browser sessions + OAuth state AND is the Ed25519 issuer key coven-web
# signs its `amr=webauthn` identity tokens with (RFC-0119). It MUST be stable and
# its public key registered in coven's trust (COVEN_TRUST_ISSUERS as
# `<origin>=ed25519:<pubkey-hex>`), or in-browser promote/yank cannot verify.
# A missing seed falls back to an ephemeral per-boot key (sessions reset each
# restart, and — without a matching trust entry — 2FA promote/yank will not work).
WEB_SEED_FILE="$SEED_DIR/coven-web-signing.seed"
if [ -n "${COVEN_WEB_SIGNING_SEED:-}" ]; then
    if [ "${#COVEN_WEB_SIGNING_SEED}" -ne 64 ]; then
        echo "entrypoint: COVEN_WEB_SIGNING_SEED must be exactly 64 hex characters, got ${#COVEN_WEB_SIGNING_SEED}" >&2
        exit 1
    fi
    printf '%s\n' "$COVEN_WEB_SIGNING_SEED" > "$WEB_SEED_FILE"
    unset COVEN_WEB_SIGNING_SEED
else
    od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > "$WEB_SEED_FILE"
fi
chown coven:coven "$WEB_SEED_FILE"; chmod 0400 "$WEB_SEED_FILE"

# "Log in with GitHub" (RFC-0010). The client ID is public (a coven-web arg); the
# client secret is coven-web's `github_client_secret` SecretStore entry, from the
# COVEN_GH_CLIENT_SECRET Fly secret via a tmpfs file. Enabled only when BOTH are
# present; otherwise coven-web runs passkey-only.
GH_GRANTS=""
GH_APP_ARGS=""
if [ -n "${COVEN_GH_CLIENT_ID:-}" ] && [ -n "${COVEN_GH_CLIENT_SECRET:-}" ]; then
    GH_SECRET_FILE="$SEED_DIR/coven-web-gh-secret"
    printf '%s' "$COVEN_GH_CLIENT_SECRET" > "$GH_SECRET_FILE"
    unset COVEN_GH_CLIENT_SECRET
    chown coven:coven "$GH_SECRET_FILE"; chmod 0400 "$GH_SECRET_FILE"
    GH_GRANTS="--net github.com:443 --net api.github.com:443 --secret-file github_client_secret=$GH_SECRET_FILE"
    GH_APP_ARGS="$COVEN_GH_CLIENT_ID"
    echo "entrypoint: GitHub login enabled (client id ${COVEN_GH_CLIENT_ID})"
fi

# The registry store (Fly volume, arrives root-owned → hand to the runtime user).
mkdir -p "$ROOT"; chown coven:coven "$ROOT"

# The web UI's served Dir lives ON the volume so its small server-side state
# (registered passkey `_wa_cred.json`, challenge/nonce markers) survives restarts
# and deploys; the static assets are refreshed from the image each boot. The `_web`
# name is deliberate: coven's store scan skips top-level `_`-prefixed entries, so
# the web dir shares coven's root without appearing in the registry index.
WEB_DIR="$ROOT/_web"
mkdir -p "$WEB_DIR"
cp -R "$DIST"/. "$WEB_DIR"/
chown -R coven:coven "$WEB_DIR"

# The deep-recursion thread reserves a 4 GiB (lazily committed) stack; a small VM's
# default overcommit heuristic refuses it and the process crash-loops before binding.
echo 1 > /proc/sys/vm/overcommit_memory || true

HOME=/home/coven
export HOME

# ---- 1. coven-serve (registry) on loopback ----------------------------------
# COVEN_TRUST_ISSUERS: whitespace/newline-separated static `--trust-issuer` specs,
# each `<issuer>=<pubkey-hex>`, `<issuer>=jwks:<json>`, or `<issuer>=ed25519:<hex>`
# (RFC-0119 — the coven-web human-2FA issuer). COVEN_OIDC_ISSUERS: space-separated
# https issuer URLs discovered live at startup (`--trust-issuer-oidc`, e.g. GitHub
# Actions). Without any trust the registry is ANONYMOUS — never on a public URL.
set -- witchy coven-serve --addr "$COVEN_LOOPBACK" --root "$ROOT" --signing-key "$SEED_FILE"
for spec in ${COVEN_TRUST_ISSUERS:-}; do
    [ -n "$spec" ] && set -- "$@" --trust-issuer "$spec"
done
for iss in ${COVEN_OIDC_ISSUERS:-}; do
    [ -n "$iss" ] && set -- "$@" --trust-issuer-oidc "$iss"
done
setpriv --reuid coven --regid coven --init-groups "$@" &
COVEN_PID=$!

# ---- 2. coven-web (public UI + same-origin proxy) ---------------------------
# Grants: the served web Dir, the public listener, the loopback upstream, GitHub
# OAuth (when enabled), and its Ed25519 signing key. Positional args:
# <listen> <upstream> <origin> <rp-id> [<gh-client-id>].
# shellcheck disable=SC2086 # GH_GRANTS / GH_APP_ARGS are intentionally word-split.
setpriv --reuid coven --regid coven --init-groups \
    witchy sandbox --dir "$WEB_DIR" --net "$WEB_ADDR" --net "$COVEN_LOOPBACK" \
    $GH_GRANTS --signing-key "$WEB_SEED_FILE" \
    "$APP" "$WEB_ADDR" "$COVEN_LOOPBACK" "$ORIGIN" "$RP_ID" $GH_APP_ARGS &
WEB_PID=$!

echo "entrypoint: coven pid $COVEN_PID on $COVEN_LOOPBACK (root $ROOT); coven-web pid $WEB_PID on $WEB_ADDR (origin $ORIGIN, rp id $RP_ID)"

# ---- supervise: if EITHER exits, tear the other down and exit nonzero --------
while kill -0 "$COVEN_PID" 2>/dev/null && kill -0 "$WEB_PID" 2>/dev/null; do
    sleep 2
done
echo "entrypoint: a server process exited; shutting down the origin (Fly will restart)." >&2
kill "$COVEN_PID" "$WEB_PID" 2>/dev/null || true
wait "$COVEN_PID" 2>/dev/null || true
wait "$WEB_PID" 2>/dev/null || true
exit 1
