#!/usr/bin/env bash
# Fast RFC-0165 hook self-test: tiny fixtures only, no census freeze.
set -euo pipefail
cd "$(dirname "$0")/.."

target_dir="${CARGO_TARGET_DIR:?CARGO_TARGET_DIR must name the isolated RFC-0165 target}"
binary="$target_dir/debug/witchy"
[ -x "$binary" ] || { echo "test-rfc0165-freeze-hook: missing $binary (build the CLI first)" >&2; exit 1; }
tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-rfc0165-hook.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/examples" "$tmp/std" "$tmp/projects/external/src" \
    "$tmp/projects/nondeterministic/src" "$tmp/tests/fixtures/rfc0162-sec"
cat >"$tmp/examples/tiny.witchy" <<'EOF'
fn main(console: Console):
    console.print("tiny-valid")
EOF
cat >"$tmp/std/tiny.witchy" <<'EOF'
pub fn helper() -> String:
    "library"
EOF
cat >"$tmp/projects/external/src/main.witchy" <<'EOF'
fn main(console: Console):
    console.print("external-not-run")
EOF
cat >"$tmp/projects/nondeterministic/src/main.witchy" <<'EOF'
fn main(console: Console):
    console.print("nondeterministic-not-run")
EOF
cat >"$tmp/tests/fixtures/rfc0162-sec/tiny_invalid.witchy" <<'EOF'
fn main() -> Int:
    "not-an-int"
EOF
metadata="$tmp/fixtures.json"
cat >"$metadata" <<'EOF'
{
  "schema_version": 2,
  "fixtures": {
    "examples/tiny.witchy": {"kind": "runnable-deterministic", "freeze_class": "v1-direct", "input_domain": {"argv": []}, "environment_domain": {"host": "self-test"}},
    "std/tiny.witchy": {"kind": "library", "freeze_class": "v1-materialized", "input_domain": {"compile_input": "source-only"}, "environment_domain": {"host": "self-test"}},
    "projects/external/src/main.witchy": {"kind": "external", "freeze_class": "v2-required", "input_domain": {"external_fixture": "unavailable"}, "environment_domain": {"host": "self-test"}},
    "projects/nondeterministic/src/main.witchy": {"kind": "runnable-nondeterministic", "freeze_class": "v1-direct", "input_domain": {"event_stream": "unbound"}, "environment_domain": {"host": "self-test"}},
    "tests/fixtures/rfc0162-sec/tiny_invalid.witchy": {"kind": "intentionally-invalid", "freeze_class": null, "input_domain": {"compile_input": "source-only"}, "environment_domain": {"host": "self-test"}}
  }
}
EOF
run_hook() {
    WITCHY_SAFEWIR_ROOT="$tmp" WITCHY_SAFEWIR_FIXTURE_METADATA="$metadata" \
        WITCHY_SAFEWIR_WITCHY_BIN="$binary" scripts/rfc0165-freeze-hook.sh \
        --source "$1" --profile normal
}
valid="$(run_hook examples/tiny.witchy)"
library="$(run_hook std/tiny.witchy)"
external="$(run_hook projects/external/src/main.witchy)"
nondeterministic="$(run_hook projects/nondeterministic/src/main.witchy)"
invalid="$(run_hook tests/fixtures/rfc0162-sec/tiny_invalid.witchy)"
if native_error="$(WITCHY_SAFEWIR_ROOT="$tmp" WITCHY_SAFEWIR_FIXTURE_METADATA="$metadata" \
    WITCHY_SAFEWIR_WITCHY_BIN="$binary" scripts/rfc0165-freeze-hook.sh \
    --source examples/tiny.witchy --profile native 2>&1)"; then
    echo "test-rfc0165-freeze-hook: native profile unexpectedly used a release route" >&2
    exit 1
fi
case "$native_error" in
    *"no distinct native-feature/runtime route"*) ;;
    *) echo "test-rfc0165-freeze-hook: native profile failed for the wrong reason: $native_error" >&2; exit 1 ;;
esac
HOOK_VALID="$valid" HOOK_LIBRARY="$library" HOOK_EXTERNAL="$external" \
HOOK_NONDETERMINISTIC="$nondeterministic" HOOK_INVALID="$invalid" python3 - <<'PY'
import base64
import hashlib
import json
import os

def check(raw, classification, admitted, fixture_kind, step_count, freeze_class):
    item = json.loads(raw)
    trace = item["trace"]
    record = trace["record"]
    check_admitted = record["steps"][0]["exit_status"] == 0
    assert item["classification"] == classification
    assert item["adapter_admitted"] is check_admitted is admitted
    assert item["freeze_class"] == freeze_class
    assert record["fixture_kind"] == fixture_kind
    assert len(record["steps"]) == step_count
    assert "ordered_host_effects" not in record["steps"][0]
    assert "merged_output_base64" in record["steps"][0]
    assert record["steps"][0]["argv"][0] == "witchy"
    assert record["route"] == "witchy-current-adapter-ordinary-production-cli"
    assert "binary_path" not in record["environment_domain"]["actual"]
    assert len(record["environment_domain"]["actual"]["binary_sha256"]) == 64
    digest = hashlib.sha256(json.dumps(record, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    assert trace["digest"] == digest
    return item

valid = check(os.environ["HOOK_VALID"], "known-valid", True, "runnable-deterministic", 2, "v1-direct")
assert b"tiny-valid" in base64.b64decode(valid["trace"]["record"]["steps"][1]["merged_output_base64"])
check(os.environ["HOOK_LIBRARY"], "known-valid", True, "library", 1, "v1-materialized")
check(os.environ["HOOK_EXTERNAL"], "external-fixture", True, "external", 1, "v2-required")
check(os.environ["HOOK_NONDETERMINISTIC"], "external-fixture", True, "runnable-nondeterministic", 1, "v1-direct")
check(os.environ["HOOK_INVALID"], "intentionally-invalid", False, "intentionally-invalid", 1, None)
PY
printf '%s\n' "test-rfc0165-freeze-hook: ok (admission, merged stream, explicit fixture kinds)"
