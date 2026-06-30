#!/usr/bin/env bash
# The performance suite: each benchmark is a PAIR of programs (witchy / Go,
# C# when dotnet is present) doing identical work. The witchy leg runs the
# COMPILED WASM tier (`witchy sandbox`, compile cache warm), because that is
# the deployment tier the plan optimizes. Run from anywhere:
#   bench/run.sh [bench…]    (default: all)
set -euo pipefail
cd "$(dirname "$0")"
WITCHY=${WITCHY:-../target/release/witchy}
(cd .. && cargo build --release --quiet)
mkdir -p bin
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
  legs=("$WITCHY sandbox ${b}.witchy")
  [ -f "bin/${b}_go" ] && legs=("bin/${b}_go" "${legs[@]}")
  if command -v dotnet >/dev/null 2>&1 && [ -f "${b}.cs" ]; then
    legs+=("dotnet run --project ${b}")
  fi
  hyperfine --warmup 3 --style basic "${legs[@]}"
done
