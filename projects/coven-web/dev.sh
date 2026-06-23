#!/usr/bin/env bash
#
# dev.sh — launch a local coven registry + coven-web UI for development.
#
# Starts two background servers (coven on :8787, coven-web on :8080), seeds a
# sample rune so there's something to browse, and opens the browser. PIDs and
# logs live under the store dir so you can stop/reset across invocations.
#
#   ./dev.sh up           # build-if-needed, launch both, seed, open browser
#   ./dev.sh down         # stop both
#   ./dev.sh restart      # down + up
#   ./dev.sh reset        # stop, wipe the registry store, relaunch fresh
#   ./dev.sh status       # what's running
#   ./dev.sh publish      # publish a sample rune into a running coven
#   ./dev.sh logs         # tail both server logs
#
# Options: --web HOST:PORT  --coven HOST:PORT  --store DIR
#          --no-browser  --no-seed  --build
#          --name ns/name  --version X.Y.Z   (for `publish`)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/../.." && pwd)"

# --- defaults (override with flags) ---
WEB_ADDR="127.0.0.1:8080"
COVEN_ADDR="127.0.0.1:8787"
STORE="/tmp/coven-dev"
OPEN_BROWSER=1
SEED_SAMPLE=1
DO_BUILD=0
PKG_NAME="demo/hello"
PKG_VERSION="1.0.0"
CMD=""

usage() { sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        up|start|down|stop|restart|reset|status|publish|logs) CMD="$1"; shift ;;
        --web)        WEB_ADDR="$2"; shift 2 ;;
        --coven)      COVEN_ADDR="$2"; shift 2 ;;
        --store)      STORE="$2"; shift 2 ;;
        --name)       PKG_NAME="$2"; shift 2 ;;
        --version)    PKG_VERSION="$2"; shift 2 ;;
        --no-browser) OPEN_BROWSER=0; shift ;;
        --no-seed)    SEED_SAMPLE=0; shift ;;
        --build)      DO_BUILD=1; shift ;;
        -h|--help)    usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
    esac
done
CMD="${CMD:-up}"

SEED="$STORE/seed.hex"
COVEN_PIDF="$STORE/coven.pid"
WEB_PIDF="$STORE/web.pid"
COVEN_LOG="$STORE/coven.log"
WEB_LOG="$STORE/web.log"
# WebAuthn ties the RP id + origin to the *access host*, and an IP literal (127.0.0.1)
# cannot use the `localhost` RP id — so when bound to a loopback address, drive the
# UI (and the server's expected origin/rp_id) through `localhost`, which resolves to
# the same socket. A real --web host is used verbatim.
WEB_PORT="${WEB_ADDR##*:}"
case "${WEB_ADDR%:*}" in
    127.0.0.1|0.0.0.0|localhost|"") WEB_HOST="localhost" ;;
    *) WEB_HOST="${WEB_ADDR%:*}" ;;
esac
WEB_URL="http://$WEB_HOST:$WEB_PORT"
RP_ID="$WEB_HOST"
COVEN_URL="http://$COVEN_ADDR"
DIST="$REPO/projects/coven-web/web/dist"
BIN=""

die() { echo "error: $*" >&2; exit 1; }

pick_bin() {
    [ "$DO_BUILD" = 1 ] && { echo "building release witchy…"; ( cd "$REPO" && cargo build --release ); }
    if [ -x "$REPO/target/release/witchy" ]; then BIN="$REPO/target/release/witchy"
    elif command -v witchy >/dev/null 2>&1; then BIN="$(command -v witchy)"
    else die "witchy binary not found — run with --build, or 'cargo build --release'"; fi
}

alive() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }

wait_up() { # $1=url
    for _ in $(seq 1 60); do
        curl -fsS -o /dev/null "$1" 2>/dev/null && return 0
        sleep 0.3
    done
    return 1
}

open_url() {
    if command -v open >/dev/null 2>&1; then open "$1"
    elif command -v xdg-open >/dev/null 2>&1; then xdg-open "$1"
    else echo "  open this in your browser: $1"; fi
}

ensure_seed() {
    mkdir -p "$STORE"
    [ -f "$SEED" ] && return 0
    if command -v openssl >/dev/null 2>&1; then openssl rand -hex 32 > "$SEED"
    else head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$SEED"; fi
}

start_coven() {
    alive "$COVEN_PIDF" && { echo "coven already running (pid $(cat "$COVEN_PIDF"))"; return 0; }
    echo "starting coven   on $COVEN_ADDR (store: $STORE)"
    ( cd "$REPO" && exec "$BIN" sandbox --dir "$STORE" --net "$COVEN_ADDR" --signing-key "$SEED" \
        projects/coven/src/coven.witchy "$COVEN_ADDR" ) >"$COVEN_LOG" 2>&1 &
    echo $! > "$COVEN_PIDF"
}

start_web() {
    alive "$WEB_PIDF" && { echo "coven-web already running (pid $(cat "$WEB_PIDF"))"; return 0; }
    echo "starting coven-web on $WEB_ADDR -> $COVEN_ADDR"
    ( cd "$REPO" && exec "$BIN" sandbox --dir "$DIST" --net "$WEB_ADDR" --net "$COVEN_ADDR" --signing-key "$SEED" \
        projects/coven-web/src/coven_web.witchy "$WEB_ADDR" "$COVEN_ADDR" "$WEB_URL" "$RP_ID" ) >"$WEB_LOG" 2>&1 &
    echo $! > "$WEB_PIDF"
}

publish_sample() { # uses PKG_NAME / PKG_VERSION
    local manifest src body
    # JSON string values: \" for the TOML's own quotes, \n for newlines.
    manifest="[rune]\nname = \\\"$PKG_NAME\\\"\nversion = \\\"$PKG_VERSION\\\"\n\n[capabilities]\nruntime = [\\\"Console\\\"]\n"
    src="pub fn id(x: Int) -> Int:\n    x\n"
    body="{\"manifest_toml\":\"$manifest\",\"uploaded_by\":\"ci\",\"source\":{\"files\":[[\"witchy.toml\",\"$manifest\"],[\"src/hello.witchy\",\"$src\"]]}}"
    if curl -fsS -X POST "$COVEN_URL/coven/publish" -H 'content-type: application/json' -d "$body" >/dev/null 2>&1; then
        echo "published $PKG_NAME@$PKG_VERSION"
    else
        echo "publish failed — is coven running? ($COVEN_URL)"
    fi
}

seed_examples() { # populate the registry with witchy's real example runes
    command -v node >/dev/null 2>&1 || { echo "node not found — skipping example seeding"; return 0; }
    echo "seeding example runes into coven…"
    COVEN_URL="$COVEN_URL" node "$REPO/projects/coven-web/seed-examples.mjs" 2>&1 | tail -1
}

stop_one() { # $1=pidfile  $2=label
    if alive "$1"; then
        local p; p="$(cat "$1")"
        if kill "$p" 2>/dev/null; then echo "stopped $2 (pid $p)"; fi
    fi
    rm -f "$1"
}

do_up() {
    pick_bin
    [ -d "$DIST" ] || die "missing web bundle: $DIST (build it: projects/coven-web/web/build.sh)"
    ensure_seed
    start_coven
    wait_up "$COVEN_URL/coven/index" || { echo "coven did not come up; last log lines:"; tail -n 20 "$COVEN_LOG" 2>/dev/null; exit 1; }
    [ "$SEED_SAMPLE" = 1 ] && publish_sample
    [ "$SEED_SAMPLE" = 1 ] && seed_examples
    start_web
    wait_up "$WEB_URL/" || { echo "coven-web did not come up; last log lines:"; tail -n 20 "$WEB_LOG" 2>/dev/null; exit 1; }
    echo
    echo "  coven-web:  $WEB_URL"
    echo "  coven api:  $COVEN_URL/coven/index"
    echo "  logs:       $COVEN_LOG"
    echo "              $WEB_LOG"
    echo "  stop:       $0 down --store $STORE"
    echo
    [ "$OPEN_BROWSER" = 1 ] && open_url "$WEB_URL"
    return 0
}

do_down() { stop_one "$WEB_PIDF" coven-web; stop_one "$COVEN_PIDF" coven; }

do_reset() {
    [ -n "$STORE" ] && [ "$STORE" != "/" ] || die "refusing to wipe unsafe store path: '$STORE'"
    do_down
    echo "wiping store $STORE"
    rm -rf "$STORE"
    do_up
}

do_status() {
    if alive "$COVEN_PIDF"; then echo "coven:     running (pid $(cat "$COVEN_PIDF"))  $COVEN_URL"; else echo "coven:     stopped"; fi
    if alive "$WEB_PIDF";   then echo "coven-web: running (pid $(cat "$WEB_PIDF"))  $WEB_URL"; else echo "coven-web: stopped"; fi
}

do_logs() {
    local f=()
    [ -f "$COVEN_LOG" ] && f+=("$COVEN_LOG")
    [ -f "$WEB_LOG" ]   && f+=("$WEB_LOG")
    [ ${#f[@]} -gt 0 ] || die "no logs under $STORE — start it first ($0 up)"
    tail -n 40 -F "${f[@]}"
}

case "$CMD" in
    up|start) do_up ;;
    down|stop) do_down ;;
    restart)  do_down; do_up ;;
    reset)    do_reset ;;
    status)   do_status ;;
    publish)  publish_sample ;;
    logs)     do_logs ;;
    *) die "unknown command: $CMD" ;;
esac
