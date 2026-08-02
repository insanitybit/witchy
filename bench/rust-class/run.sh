#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
bench_dir="$root/bench/rust-class"
mode=${1:---check}
case "$mode" in
  --check|--measure|--enforce) ;;
  *) echo "usage: bench/rust-class/run.sh [--check|--measure|--enforce]" >&2; exit 2 ;;
esac

WITCHY=${WITCHY:-$root/target/release/witchy}
RUSTC=${RUSTC:-rustc}
samples=${WITCHY_RUST_SAMPLES:-7}
build_dir=${WITCHY_RUST_BUILD_DIR:-$bench_dir/bin}
mkdir -p "$build_dir"

rust_flags=(
  --edition 2024
  -C opt-level=3
  -C codegen-units=1
  -C llvm-args=-vectorize-loops=false
  -C llvm-args=-vectorize-slp=false
)

cases=(
  scalar_int
  scalar_float
  packed_records
  list_pipeline
  closed_sum
  generic_helpers
  destination_record
  recursive_values
)

if [ ! -x "$WITCHY" ]; then
  (cd "$root" && cargo build --release --quiet)
fi

echo "witchy=$($WITCHY --version | head -1)"
echo "rustc=$($RUSTC --version)"
echo "host=$(uname -s)-$(uname -m)"
echo "rust_flags=${rust_flags[*]}"

kernel_assembly() {
  local asm=$1
  awk '
    /^_?witchy_rust_class_kernel:/ { inside=1 }
    inside { print }
    inside && /Lfunc_end[0-9]+:/ { exit }
  ' "$asm"
}

verify_scalar_kernel() {
  local asm=$1 arch block
  arch=$(uname -m)
  block=$(kernel_assembly "$asm")
  [ -n "$block" ] || {
    echo "rust-class: measured kernel symbol missing from $asm" >&2
    return 1
  }
  case "$arch" in
    arm64|aarch64)
      if rg -i '\b(v[0-9]+\.(16b|8h|4s|2d|8b|4h|2s)|q[0-9]+|z[0-9]+)\b' "$asm" >/dev/null; then
        echo "rust-class: vector instruction/register in measured Rust translation unit $asm" >&2
        rg -i '\b(v[0-9]+\.(16b|8h|4s|2d|8b|4h|2s)|q[0-9]+|z[0-9]+)\b' "$asm" >&2
        return 1
      fi
      ;;
    x86_64|amd64)
      if rg -i '\b(ymm|zmm)[0-9]+\b|\b(v?p(add|sub|mul|and|or|xor|cmp|blend|shuf)|v?(add|sub|mul|div|min|max)(ps|pd))\b' "$asm" >/dev/null; then
        echo "rust-class: packed-vector instruction in measured Rust translation unit $asm" >&2
        rg -i '\b(ymm|zmm)[0-9]+\b|\b(v?p(add|sub|mul|and|or|xor|cmp|blend|shuf)|v?(add|sub|mul|div|min|max)(ps|pd))\b' "$asm" >&2
        return 1
      fi
      ;;
    *)
      echo "rust-class: no scalar-instruction verifier for architecture $arch" >&2
      return 1
      ;;
  esac
}

field() {
  local name=$1
  awk -F= -v wanted="$name" '$1 == wanted { print $2; exit }'
}

expected_result() {
  case "$1" in
    scalar_int) printf '%s' 24000006 ;;
    scalar_float) printf '%s' 1 ;;
    packed_records) printf '%s' 9599879 ;;
    list_pipeline) printf '%s' 12000160 ;;
    closed_sum) printf '%s' 16833142 ;;
    generic_helpers) printf '%s' 9599942 ;;
    destination_record) printf '%s' 49999959 ;;
    recursive_values) printf '%s' 9999190 ;;
    *) echo "rust-class: no independent expected result for $1" >&2; return 1 ;;
  esac
}

run_witchy() {
  "$WITCHY" sandbox "$bench_dir/$1.witchy"
}

run_rust() {
  "$build_dir/$1-rust"
}

best_ns() {
  local leg=$1 case_name=$2 best='' output result ns i
  for ((i=0; i<samples; i++)); do
    if [ "$leg" = witchy ]; then output=$(run_witchy "$case_name"); else output=$(run_rust "$case_name"); fi
    result=$(printf '%s\n' "$output" | field result)
    ns=$(printf '%s\n' "$output" | field bench_ns)
    [ "$result" = "$3" ] || { echo "rust-class: $case_name $leg result changed: $result != $3" >&2; return 1; }
    [[ "$ns" =~ ^[0-9]+$ ]] || { echo "rust-class: $case_name $leg emitted invalid bench_ns=$ns" >&2; return 1; }
    if [ -z "$best" ] || [ "$ns" -lt "$best" ]; then best=$ns; fi
  done
  printf '%s' "$best"
}

ratios=''
for case_name in "${cases[@]}"; do
  rust_src="$bench_dir/$case_name.rs"
  rust_bin="$build_dir/$case_name-rust"
  rust_asm="$build_dir/$case_name.s"
  (
    cd "$build_dir"
    "$RUSTC" "${rust_flags[@]}" --emit link="$rust_bin",asm="$rust_asm" "$rust_src"
  )
  verify_scalar_kernel "$rust_asm"

  witchy_output=$(run_witchy "$case_name")
  rust_output=$(run_rust "$case_name")
  witchy_result=$(printf '%s\n' "$witchy_output" | field result)
  rust_result=$(printf '%s\n' "$rust_output" | field result)
  expected=$(expected_result "$case_name")
  [ "$witchy_result" = "$expected" ] && [ "$rust_result" = "$expected" ] || {
    echo "rust-class: $case_name result mismatch: expected=$expected witchy=$witchy_result rust=$rust_result" >&2
    exit 1
  }
  echo "$case_name result=$witchy_result scalar_rust=verified"

  if [ "$mode" != --check ]; then
    witchy_ns=$(best_ns witchy "$case_name" "$witchy_result")
    rust_ns=$(best_ns rust "$case_name" "$rust_result")
    ratio=$(awk -v w="$witchy_ns" -v r="$rust_ns" 'BEGIN { printf "%.6f", w/r }')
    ratios="$ratios $ratio"
    printf '%s witchy_ns=%s rust_ns=%s ratio=%sx\n' "$case_name" "$witchy_ns" "$rust_ns" "$ratio"
    if [ "$mode" = --enforce ] && ! awk -v ratio="$ratio" 'BEGIN { exit !(ratio <= 1.50) }'; then
      echo "rust-class: $case_name exceeds the RFC-0111 1.50x per-case ceiling" >&2
      exit 1
    fi
  fi
done

if [ "$mode" != --check ]; then
  geomean=$(printf '%s\n' "$ratios" | awk '{ for (i=1; i<=NF; i++) sum += log($i); printf "%.6f", exp(sum/NF) }')
  echo "geomean_ratio=${geomean}x"
  if [ "$mode" = --enforce ] && ! awk -v ratio="$geomean" 'BEGIN { exit !(ratio <= 1.25) }'; then
    echo "rust-class: geometric mean exceeds the RFC-0111 1.25x ceiling" >&2
    exit 1
  fi
fi
