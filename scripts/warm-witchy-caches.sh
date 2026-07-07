#!/usr/bin/env bash
# Warm the wasmtime compile caches for the EMBEDDED witchy programs (pm,
# coven) before a test run. Run automatically by nextest as a setup script
# (.config/nextest.toml) ahead of the e2e binary.
#
# Why: the e2e suite spawns `witchy pm ...` / `witchy coven-serve` subprocesses
# dozens of times. Each spawn compiles projects/pm / projects/coven to wasm
# unless the on-disk caches (~/.cache/witchy/{src,aot,wasm}) are warm — and
# those caches are keyed on the binary's mtime+size, so EVERY rebuild (i.e.
# every merge) invalidates them. Until this script existed, the first gate
# after a rebuild paid the same embedded-program compile in every spawned
# subprocess. Warming once up front turns all of those into cache hits
# (Module::deserialize — microseconds).
#
# Best-effort by design: this script ALWAYS exits 0. If the binary is missing
# or broken, the tests themselves will say so with a real error; a warm
# failure must never fail the run. It also bounds all its own waiting, so the
# nextest setup-script timeout never fires first.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 0

target_dir="${CARGO_TARGET_DIR:-target}"
BIN="$target_dir/debug/witchy"
[ -x "$BIN" ] || { echo "warm-witchy-caches: no binary at $BIN; skipping" >&2; exit 0; }

home="$(mktemp -d -t witchy-warm)"
cleanup() {
    [ -n "${srv_pid:-}" ] && kill "$srv_pid" 2>/dev/null
    [ -n "${srv_pid:-}" ] && wait "$srv_pid" 2>/dev/null
    rm -rf "$home"
}
trap cleanup EXIT

# 1. The pm front-end (projects/pm): any subcommand compiles it. `pm list` in
#    an empty home exits nonzero (no project) — the compile, and therefore the
#    cache write, happens regardless.
WITCHY_HOME="$home/pm" "$BIN" pm list >/dev/null 2>&1 || true

# 2. The coven registry server (projects/coven + siblings): compiles on
#    startup, then binds. Spawn it on a free port, wait for the listener
#    (listening == compile finished == caches written), then kill it.
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')" || { echo "warm-witchy-caches: no python3; pm warmed, skipping coven" >&2; exit 0; }
regroot="$home/regroot"
mkdir -p "$regroot"
printf '%064d' 1 >"$regroot/root.seed"
WITCHY_HOME="$home/srv" "$BIN" coven-serve --addr "127.0.0.1:$port" \
    --root "$regroot" --signing-key "$regroot/root.seed" >/dev/null 2>&1 &
srv_pid=$!
# 120s budget: a cold debug-build compile of coven takes ~10-30s; the bound
# only exists so a wedged server can't hang the suite start.
for _ in $(seq 1 600); do
    kill -0 "$srv_pid" 2>/dev/null || break        # server exited (error path)
    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
        exec 3>&- 3<&-
        break
    fi
    sleep 0.2
done

echo "warm-witchy-caches: embedded pm+coven caches warmed for $BIN" >&2
exit 0
