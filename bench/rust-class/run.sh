#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
bench_dir="$root/bench/rust-class"
mode=${1:---check}
[ "$#" -eq 0 ] || shift
report_path=''
case_rows=''
report_tmp=''
owned_target=''
owned_tmp_root=''

usage() {
  echo "usage: bench/rust-class/run.sh --check" >&2
  echo "       bench/rust-class/run.sh --measure --report PATH" >&2
  echo "       bench/rust-class/run.sh --enforce --report PATH" >&2
  echo "       bench/rust-class/run.sh --verify-report PATH" >&2
}

case "$mode" in
  --check) [ "$#" -eq 0 ] || { usage; exit 2; } ;;
  --measure|--enforce)
    [ "$#" -eq 2 ] && [ "$1" = --report ] && [ -n "$2" ] || { usage; exit 2; }
    report_path=$2
    ;;
  --verify-report)
    [ "$#" -eq 1 ] && [ -n "$1" ] || { usage; exit 2; }
    report_path=$1
    ;;
  *) usage; exit 2 ;;
esac

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

require_jq() {
  command -v jq >/dev/null 2>&1 || {
    echo "rust-class: jq is required for benchmark reports" >&2
    return 1
  }
}

verify_report() {
  local report=$1
  require_jq
  [ -f "$report" ] || { echo "rust-class: report not found: $report" >&2; return 1; }
  jq -e '
    def exact_keys($wanted): (keys | sort) == ($wanted | sort);
    def sha256: type == "string" and test("^[0-9a-f]{64}$");
    def commit: type == "string" and test("^[0-9a-f]{40}$");
    def nonempty: type == "string" and length > 0;
    def positive_integer: type == "number" and floor == . and . > 0;
    def ratio_matches:
      (((.witchy_ns / .rust_ns) - .ratio) | fabs) <= 0.000001;
    . as $report
    | exact_keys(["schema_version", "benchmark", "git", "witchy", "rustc",
                  "host", "rust_flags", "samples", "scalar_verifier",
                  "thresholds", "cases", "geomean_ratio", "passed"])
    and .schema_version == 1
    and .benchmark == "rfc0111-rust-class"
    and (.git | exact_keys(["commit", "tree_clean"])
         and (.commit | commit) and .tree_clean == true)
    and (.witchy | exact_keys(["version", "binary_sha256"])
         and (.version | nonempty)
         and (.version | contains("(commit " + $report.git.commit + ")"))
         and (.binary_sha256 | sha256))
    and (.rustc | exact_keys(["verbose_version"])
         and (.verbose_version | startswith("rustc ")))
    and (.host | exact_keys(["os", "architecture", "cpu"])
         and (.os | nonempty) and (.architecture | nonempty) and (.cpu | nonempty))
    and .rust_flags == ["--edition", "2024", "-C", "opt-level=3", "-C",
                        "codegen-units=1", "-C", "llvm-args=-vectorize-loops=false",
                        "-C", "llvm-args=-vectorize-slp=false"]
    and .samples == 7
    and (.scalar_verifier
         | exact_keys(["architecture", "scope", "status"])
         and (.architecture == "arm64" or .architecture == "aarch64")
         and .scope == "translation-unit" and .status == "verified")
    and .scalar_verifier.architecture == .host.architecture
    and (.thresholds | exact_keys(["case_max", "geomean_max"])
         and .case_max == 1.5 and .geomean_max == 1.25)
    and ([.cases[].name] == ["scalar_int", "scalar_float", "packed_records",
                             "list_pipeline", "closed_sum", "generic_helpers",
                             "destination_record", "recursive_values"])
    and ([.cases[].result] == [24000006, 1, 9599879, 12000160, 16833142,
                               9599942, 49999959, 9999190])
    and all(.cases[];
      exact_keys(["name", "result", "witchy_ns", "rust_ns", "ratio"])
      and (.result | type == "number" and floor == .)
      and (.witchy_ns | positive_integer)
      and (.rust_ns | positive_integer)
      and (.ratio | type == "number" and . > 0)
      and ratio_matches)
    and (.geomean_ratio | type == "number" and . > 0)
    and (((([.cases[].ratio | log] | add / length | exp) - .geomean_ratio)
          | fabs) <= 0.000001)
    and (.passed | type == "boolean")
    and (.passed == ((all(.cases[]; .ratio <= $report.thresholds.case_max))
                     and (.geomean_ratio <= $report.thresholds.geomean_max)))
  ' "$report" >/dev/null || {
    echo "rust-class: report failed schema or invariant verification: $report" >&2
    return 1
  }
  echo "rust-class: verified report $report"
}

if [ "$mode" = --verify-report ]; then
  verify_report "$report_path"
  exit 0
fi

witchy_override=${WITCHY+x}
WITCHY=${WITCHY:-}
RUSTC=${RUSTC:-rustc}
samples=${WITCHY_RUST_SAMPLES:-7}
target_dir=${CARGO_TARGET_DIR:-$root/target}
case "$target_dir" in /*) ;; *) target_dir="$root/$target_dir" ;; esac
build_dir=${WITCHY_RUST_BUILD_DIR:-$target_dir/rust-class}
mkdir -p "$build_dir"

case "$samples" in
  ''|*[!0-9]*) echo "rust-class: WITCHY_RUST_SAMPLES must be a positive integer" >&2; exit 2 ;;
  *) [ "$samples" -gt 0 ] || { echo "rust-class: WITCHY_RUST_SAMPLES must be a positive integer" >&2; exit 2; } ;;
esac
if [ "$mode" != --check ] && [ "$samples" -ne 7 ]; then
  echo "rust-class: reportable runs require exactly seven samples" >&2
  exit 2
fi
if [ "$mode" != --check ] && [ "$witchy_override" = x ]; then
  echo "rust-class: reportable runs reject WITCHY; the harness builds clean HEAD itself" >&2
  exit 2
fi

head=$(git -C "$root" rev-parse --verify HEAD)
tree_clean=true
if ! git -C "$root" diff --quiet --ignore-submodules -- \
    || ! git -C "$root" diff --cached --quiet --ignore-submodules -- \
    || [ -n "$(git -C "$root" ls-files --others --exclude-standard)" ]; then
  tree_clean=false
fi
if [ "$mode" != --check ] && [ "$tree_clean" != true ]; then
  echo "rust-class: reportable runs require a clean worktree" >&2
  exit 1
fi

host_os=$(uname -s)
host_arch=$(uname -m)
scalar_verifier_available=false
case "$host_arch" in
  arm64|aarch64) scalar_verifier_available=true ;;
  *)
    if [ "$mode" != --check ]; then
      echo "rust-class: reportable scalar evidence is not implemented for architecture $host_arch" >&2
      exit 1
    fi
    echo "rust-class: $host_arch check is correctness-only; scalar certification is unavailable and this run is not performance evidence" >&2
    ;;
esac
case "$host_os" in
  Darwin) host_cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || sysctl -n hw.model) ;;
  Linux) host_cpu=$(awk -F: '/^model name[[:space:]]*:/ { sub(/^[[:space:]]+/, "", $2); print $2; exit }' /proc/cpuinfo) ;;
  *) host_cpu=$(uname -p) ;;
esac
[ -n "$host_cpu" ] || host_cpu=unknown
rustc_verbose=$("$RUSTC" -vV)

cleanup() {
  local links=''
  if [ -n "$case_rows" ] && [ -f "$case_rows" ]; then
    rm -f -- "$case_rows" || echo "rust-class: could not remove temporary case rows $case_rows" >&2
  fi
  if [ -n "$report_tmp" ] && [ -f "$report_tmp" ]; then
    rm -f -- "$report_tmp" || echo "rust-class: could not remove temporary report $report_tmp" >&2
  fi
  [ -n "$owned_target" ] || return 0
  case "$owned_target" in
    "$owned_tmp_root"/witchy-rust-class-build-*) ;;
    *)
      echo "rust-class: refusing to clean unexpected build target $owned_target" >&2
      return 0
      ;;
  esac
  if [ -L "$owned_target" ] || ! links=$(find -P "$owned_target" -type l -print -quit); then
    echo "rust-class: refusing to clean unverified build target $owned_target" >&2
  elif [ -n "$links" ]; then
    echo "rust-class: refusing to clean build target containing symlink $links" >&2
  elif [ -d "$owned_target" ]; then
    rm -rf -- "$owned_target" || echo "rust-class: could not remove build target $owned_target" >&2
  fi
}
trap cleanup EXIT

if [ "$mode" != --check ]; then
  owned_tmp_root=$(cd "${TMPDIR:-/tmp}" && pwd -P)
  owned_target=$(mktemp -d "$owned_tmp_root/witchy-rust-class-build-XXXXXX")
  [ ! -L "$owned_target" ] || { echo "rust-class: temporary build target is a symlink" >&2; exit 1; }
  owned_target=$(cd "$owned_target" && pwd -P)
  case "$owned_target" in
    "$owned_tmp_root"/witchy-rust-class-build-*) ;;
    *) echo "rust-class: unexpected temporary build target $owned_target" >&2; exit 1 ;;
  esac
  WITCHY="$owned_target/release/witchy"
  (cd "$root" && CARGO_TARGET_DIR="$owned_target" WITCHY_BUILD_COMMIT="$head" \
    cargo build -p witchy --release --quiet)
elif [ -z "$WITCHY" ]; then
  WITCHY="$target_dir/release/witchy"
  (cd "$root" && WITCHY_BUILD_COMMIT="$head" cargo build -p witchy --release --quiet)
fi
[ -x "$WITCHY" ] || { echo "rust-class: Witchy binary is not executable: $WITCHY" >&2; exit 1; }
witchy_version=$("$WITCHY" --version | head -1)
case "$witchy_version" in
  *"(commit $head)"*) ;;
  *)
    echo "rust-class: Witchy binary is not authenticated to HEAD $head: $witchy_version" >&2
    exit 1
    ;;
esac

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    echo "rust-class: sha256sum or shasum is required" >&2
    return 1
  fi
}

echo "git_head=$head tree_clean=$tree_clean"
echo "witchy=$witchy_version"
echo "rustc=$("$RUSTC" --version)"
echo "host=$host_os-$host_arch cpu=$host_cpu"
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
  local asm=$1 arch block arm_pattern
  arch=$(uname -m)
  block=$(kernel_assembly "$asm")
  [ -n "$block" ] || {
    echo "rust-class: measured kernel symbol missing from $asm" >&2
    return 1
  }
  case "$arch" in
    arm64|aarch64)
      arm_pattern='\b(v[0-9]+\.(16b|8h|4s|2d|8b|4h|2s)|q[0-9]+|z[0-9]+([.]?[bhsdq])?|p[0-9]+([.]?[bhsdq])?(/[mz])?)\b'
      if rg -i "$arm_pattern" "$asm" >/dev/null; then
        echo "rust-class: vector instruction/register in measured Rust translation unit $asm" >&2
        rg -i "$arm_pattern" "$asm" >&2
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
  local leg=$1 case_name=$2 expected=$3 best='' output result ns i
  for ((i=0; i<samples; i++)); do
    if [ "$leg" = witchy ]; then output=$(run_witchy "$case_name"); else output=$(run_rust "$case_name"); fi
    result=$(printf '%s\n' "$output" | field result)
    ns=$(printf '%s\n' "$output" | field bench_ns)
    [ "$result" = "$expected" ] || { echo "rust-class: $case_name $leg result changed: $result != $expected" >&2; return 1; }
    [[ "$ns" =~ ^[0-9]+$ ]] && [ "$ns" -gt 0 ] || { echo "rust-class: $case_name $leg emitted invalid bench_ns=$ns" >&2; return 1; }
    if [ -z "$best" ] || [ "$ns" -lt "$best" ]; then best=$ns; fi
  done
  printf '%s' "$best"
}

ratios=''
if [ "$mode" != --check ]; then
  require_jq
  case_rows=$(mktemp "${TMPDIR:-/tmp}/witchy-rust-class-cases-XXXXXX")
fi

for case_name in "${cases[@]}"; do
  rust_src="$bench_dir/$case_name.rs"
  rust_bin="$build_dir/$case_name-rust"
  rust_asm="$build_dir/$case_name.s"
  (
    cd "$build_dir"
    "$RUSTC" "${rust_flags[@]}" --emit link="$rust_bin",asm="$rust_asm" "$rust_src"
  )
  scalar_status=unavailable
  if [ "$scalar_verifier_available" = true ]; then
    verify_scalar_kernel "$rust_asm"
    scalar_status=verified
  fi

  witchy_output=$(run_witchy "$case_name")
  rust_output=$(run_rust "$case_name")
  witchy_result=$(printf '%s\n' "$witchy_output" | field result)
  rust_result=$(printf '%s\n' "$rust_output" | field result)
  expected=$(expected_result "$case_name")
  [ "$witchy_result" = "$expected" ] && [ "$rust_result" = "$expected" ] || {
    echo "rust-class: $case_name result mismatch: expected=$expected witchy=$witchy_result rust=$rust_result" >&2
    exit 1
  }
  echo "$case_name result=$witchy_result scalar_rust=$scalar_status"

  if [ "$mode" != --check ]; then
    witchy_ns=$(best_ns witchy "$case_name" "$witchy_result")
    rust_ns=$(best_ns rust "$case_name" "$rust_result")
    ratio=$(awk -v w="$witchy_ns" -v r="$rust_ns" 'BEGIN { printf "%.6f", w/r }')
    ratios="$ratios $ratio"
    printf '%s witchy_ns=%s rust_ns=%s ratio=%sx\n' "$case_name" "$witchy_ns" "$rust_ns" "$ratio"
    jq -cn --arg name "$case_name" --argjson result "$witchy_result" \
      --argjson witchy_ns "$witchy_ns" --argjson rust_ns "$rust_ns" \
      --argjson ratio "$ratio" \
      '{name: $name, result: $result, witchy_ns: $witchy_ns,
        rust_ns: $rust_ns, ratio: $ratio}' >>"$case_rows"
  fi
done

if [ "$mode" != --check ]; then
  geomean=$(printf '%s\n' "$ratios" | awk '{ for (i=1; i<=NF; i++) sum += log($i); printf "%.6f", exp(sum/NF) }')
  echo "geomean_ratio=${geomean}x"
  passed=true
  if ! awk -v ratios="$ratios" -v geomean="$geomean" 'BEGIN {
      count = split(ratios, value, " ");
      for (i = 1; i <= count; i++) if (value[i] != "" && value[i] > 1.50) exit 1;
      exit !(geomean <= 1.25)
    }'; then
    passed=false
  fi

  report_dir=$(dirname "$report_path")
  mkdir -p "$report_dir"
  report_tmp=$(mktemp "$report_path.tmp.XXXXXX")
  rust_flags_json=$(printf '%s\n' "${rust_flags[@]}" | jq -R . | jq -s .)
  cases_json=$(jq -s . "$case_rows")
  jq -Sn \
    --argjson schema_version 1 \
    --arg benchmark rfc0111-rust-class \
    --arg commit "$head" \
    --argjson tree_clean "$tree_clean" \
    --arg witchy_version "$witchy_version" \
    --arg witchy_sha256 "$(sha256_file "$WITCHY")" \
    --arg rustc_verbose "$rustc_verbose" \
    --arg host_os "$host_os" \
    --arg host_arch "$host_arch" \
    --arg host_cpu "$host_cpu" \
    --argjson rust_flags "$rust_flags_json" \
    --argjson samples "$samples" \
    --argjson cases "$cases_json" \
    --argjson geomean "$geomean" \
    --argjson passed "$passed" \
    '{schema_version: $schema_version,
      benchmark: $benchmark,
      git: {commit: $commit, tree_clean: $tree_clean},
      witchy: {version: $witchy_version, binary_sha256: $witchy_sha256},
      rustc: {verbose_version: $rustc_verbose},
      host: {os: $host_os, architecture: $host_arch, cpu: $host_cpu},
      rust_flags: $rust_flags,
      samples: $samples,
      scalar_verifier: {architecture: $host_arch, scope: "translation-unit", status: "verified"},
      thresholds: {case_max: 1.5, geomean_max: 1.25},
      cases: $cases,
      geomean_ratio: $geomean,
      passed: $passed}' >"$report_tmp"
  verify_report "$report_tmp"
  mv "$report_tmp" "$report_path"
  echo "rust-class: wrote report $report_path"

  if [ "$mode" = --enforce ] && [ "$passed" != true ]; then
    echo "rust-class: RFC-0111 thresholds failed; see $report_path" >&2
    exit 1
  fi
fi
