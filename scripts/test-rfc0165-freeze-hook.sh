#!/usr/bin/env bash
# Focused RFC-0165 hook contract test. It never builds or freezes the corpus.
set -euo pipefail
cd "$(dirname "$0")/.."

tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-rfc0165-hook.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/examples"
cat >"$tmp/examples/tiny.witchy" <<'EOF'
fn main(console: Console):
    console.print("tiny-valid")
EOF
cat >"$tmp/fake-witchy" <<'PY'
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

log = Path(os.environ["FAKE_WITCHY_LOG"])
with log.open("a", encoding="utf-8") as handle:
    handle.write(" ".join(sys.argv[1:]) + "\n")
if sys.argv[1] == "check":
    print("checked", sys.argv[2])
    raise SystemExit(0)
print("executed", sys.argv[1])
print("WITCHY_SAFEWIR_OBSERVATION_V1=" + json.dumps({
    "resource_counters": {"allocations": 1},
    "in_place_counters": {"mutations": 1},
}))
PY
chmod +x "$tmp/fake-witchy"

metadata="$tmp/contracts.json"
cat >"$metadata" <<'EOF'
{
  "schema_version": 3,
  "contracts": {
    "examples/tiny.witchy": {
      "profiles": {
        "normal": {
          "classification": "known-valid",
          "freeze_class": "v1-direct",
          "check_expectation": "admitted",
          "execution": "check-then-run",
          "input_domain": {"argv": []},
          "environment_domain": {"host": "self-test"},
          "observation_contract": "focused-v1"
        }
      }
    }
  }
}
EOF
log="$tmp/route.log"
run_hook() {
    FAKE_WITCHY_LOG="$log" WITCHY_SAFEWIR_ROOT="$tmp" \
        WITCHY_SAFEWIR_FIXTURE_METADATA="$metadata" \
        WITCHY_SAFEWIR_WITCHY_BIN="$tmp/fake-witchy" \
        scripts/rfc0165-freeze-hook.sh --source examples/tiny.witchy --profile normal
}

first="$(run_hook)"
second="$(run_hook)"
HOOK_FIRST="$first" HOOK_SECOND="$second" ROUTE_LOG="$log" python3 - <<'PY'
import hashlib
import json
import os

first = json.loads(os.environ["HOOK_FIRST"])
second = json.loads(os.environ["HOOK_SECOND"])
assert first["classification"] == "known-valid"
assert first["adapter_admitted"] is True
assert first["freeze_class"] == "v1-direct"
assert first["trace"]["digest"] == second["trace"]["digest"]
record = first["trace"]["record"]
assert [step["kind"] for step in record["steps"]] == ["check", "run"]
assert record["steps"][1]["observations"]["resource_counters"] == {"allocations": 1}
assert record["steps"][1]["observations"]["in_place_counters"] == {"mutations": 1}
expected = hashlib.sha256(json.dumps(record, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
assert first["trace"]["digest"] == expected
with open(os.environ["ROUTE_LOG"], encoding="utf-8") as handle:
    route = [line.strip() for line in handle if line.strip()]
assert route == ["check examples/tiny.witchy", "examples/tiny.witchy"] * 2
PY

bad_metadata="$tmp/missing-classification.json"
sed 's/          "classification": "known-valid",//' "$metadata" >"$bad_metadata"
if WITCHY_SAFEWIR_ROOT="$tmp" WITCHY_SAFEWIR_FIXTURE_METADATA="$bad_metadata" \
    WITCHY_SAFEWIR_WITCHY_BIN="$tmp/fake-witchy" FAKE_WITCHY_LOG="$log" \
    scripts/rfc0165-freeze-hook.sh --source examples/tiny.witchy --profile normal \
    >"$tmp/bad.out" 2>"$tmp/bad.err"; then
    echo "test-rfc0165-freeze-hook: missing classification unexpectedly accepted" >&2
    exit 1
fi
if ! rg -q "missing \['classification'\]" "$tmp/bad.err"; then
    echo "test-rfc0165-freeze-hook: missing classification failed for the wrong reason" >&2
    cat "$tmp/bad.err" >&2
    exit 1
fi

printf '%s\n' "test-rfc0165-freeze-hook: ok (explicit contract, deterministic digest, check-first route)"
