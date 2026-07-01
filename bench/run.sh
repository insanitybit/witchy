#!/usr/bin/env bash
# The performance suite: each benchmark is a PAIR of programs (witchy / Go,
# C# when dotnet is present) doing identical work. The witchy leg runs the
# COMPILED WASM tier (`witchy sandbox`, compile cache warm), because that is
# the deployment tier the plan optimizes. Run from anywhere:
#   bench/run.sh [bench…]    (default: all)
#
# Two clocks per benchmark:
#   kernel — compute time measured INSIDE the program (monotonic clock, printed
#            as `bench_ns=<n>`), excluding process/runtime startup. Isolates
#            codegen quality. Absent for `hello`, which is a cold-start probe.
#   wall   — hyperfine end-to-end (startup included); the wall-kernel gap is the
#            fixed witchy runtime-startup tax.
set -euo pipefail
cd "$(dirname "$0")"
WITCHY=${WITCHY:-../target/release/witchy}
(cd .. && cargo build --release --quiet)
mkdir -p bin

# Minimum self-reported kernel-ns over a few samples (empty if the program has
# no bench_ns bracket, e.g. hello); prints in ms.
kernel_ms() {
    local best="" ns
    for _ in 1 2 3 4 5; do
        ns=$("$@" 2>/dev/null | grep '^bench_ns=' | head -1 | cut -d= -f2) || true
        [ -n "$ns" ] || return 0
        if [ -z "$best" ] || [ "$ns" -lt "$best" ]; then best="$ns"; fi
    done
    awk "BEGIN{printf \"%.2f\", $best/1000000}"
}

BENCHES=("$@")
[ ${#BENCHES[@]} -eq 0 ] && BENCHES=(cpu listbuild strings hello parmap)
for b in "${BENCHES[@]}"; do
  [ -f "${b}.go" ] && go build -o "bin/${b}_go" "${b}.go"
  # Warm witchy's compilation cache so the measured runs are JIT-artifact hits.
  "$WITCHY" sandbox "${b}.witchy" >/dev/null 2>&1 || true
done
for b in "${BENCHES[@]}"; do
  echo
  echo "== ${b} =="
  wk=$(kernel_ms "$WITCHY" sandbox "${b}.witchy")
  if [ -n "$wk" ]; then
    gk=""; [ -f "bin/${b}_go" ] && gk=$(kernel_ms "bin/${b}_go")
    echo "  kernel: witchy ${wk}ms${gk:+   go ${gk}ms}   (compute only, no startup)"
  fi
  legs=("$WITCHY sandbox ${b}.witchy")
  [ -f "bin/${b}_go" ] && legs=("bin/${b}_go" "${legs[@]}")
  if command -v dotnet >/dev/null 2>&1 && [ -f "${b}.cs" ]; then
    legs+=("dotnet run --project ${b}")
  fi
  echo "  wall (hyperfine, startup included):"
  hyperfine --warmup 3 --style basic "${legs[@]}"
done
