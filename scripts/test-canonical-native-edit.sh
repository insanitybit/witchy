#!/usr/bin/env bash
# Hermetic command/receipt/QoS checks. Every Cargo invocation uses the mock.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-test-canonical-edit.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/repo/scripts" "$tmp/bin" "$tmp/state" "$tmp/target"
cp "$root/scripts/"{agent-check.sh,test-for-paths.sh,canonical-native-edit.sh,record-shard-receipt.sh,state-paths.sh} "$tmp/repo/scripts/"
cat > "$tmp/bin/taskpolicy" <<'SH'
#!/usr/bin/env bash
set -eu
[ "$1" = -c ] && [ "$2" = utility ]
shift 2
export MOCK_UTILITY=1
exec "$@"
SH
cat > "$tmp/bin/cargo" <<'PY'
#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
a = sys.argv[1:]
root = pathlib.Path.cwd()
record = {"args": a, "oracle": os.getenv("WITCHY_ORACLE_EXAMPLE"),
          "utility": os.getenv("MOCK_UTILITY"), "target": os.getenv("CARGO_TARGET_DIR"),
          "strip": os.getenv("CARGO_PROFILE_TEST_STRIP")}
with open(os.environ["MOCK_LOG"], "a") as log:
    log.write(json.dumps(record) + "\n")
mode = os.getenv("MOCK_FAIL", "")
packages = [{"name": p, "id": p + "@1"} for p in ["witchy-wir", "witchy-lower", "witchy"]]
if a[0] == "metadata":
    if mode == "metadata": sys.exit(19)
    print(json.dumps({"packages": packages}))
elif a[:2] == ["nextest", "list"]:
    if mode == "build": sys.exit(20)
    if mode == "empty": sys.exit(0)
    if mode == "malformed":
        print("not-json")
        sys.exit(0)
    rows = [("witchy-wir", "test", "unit"), ("witchy-wir", "test", "rfc0165_route_gate"),
            ("witchy-lower", "lib", "witchy_lower"), ("witchy", "bin", "witchy"),
            ("witchy", "test", "misc"), ("witchy", "test", "cli_subcommands"),
            ("witchy", "lib", "witchy")]
    if "codegen" in a: rows.append(("witchy-lower", "test", "codegen"))
    binaries = {}
    for package, kind, name in rows:
        if mode == "missing-" + name: continue
        key = package + "::" + kind + "::" + name
        binaries[key] = {"binary-id": key, "binary-name": name, "kind": kind,
                         "package-id": package + "@1", "binary-path": str(root / "mock-binary")}
    print(json.dumps({"rust-binaries": binaries}))
elif a[:2] == ["nextest", "run"]:
    expr = a[a.index("-E") + 1]
    if "--binaries-metadata" in a:
        # Assert files survive for all reused runs, not a cross-invocation cache.
        for flag in ["--binaries-metadata", "--cargo-metadata"]:
            json.loads(pathlib.Path(a[a.index(flag) + 1]).read_text())
        stage = "wir" if expr.startswith("package(=witchy-wir)") else "lower" if expr.startswith("package(=witchy-lower)") else "root"
    else:
        assert "witchy-runtime" in a, a
        stage = "runtime"
    if mode == stage: sys.exit(21)
    if mode == "head-change" and stage == "root":
        subprocess.run(["git", "commit", "--allow-empty", "-qm", "concurrent tip"], check=True)
else:
    raise AssertionError("unexpected Cargo invocation: " + repr(a))
PY
chmod +x "$tmp/bin/"* "$tmp/repo/scripts/"*.sh
cd "$tmp/repo"
git init -q
git symbolic-ref HEAD refs/heads/master
git config user.name "Canonical edit fixture"
git config user.email test@example.invalid
mkdir -p crates/witchy-lower/src crates/witchy-wir/src
printf '// base\n' > crates/witchy-lower/src/canonical_lower.rs
printf '// base\n' > crates/witchy-wir/src/canonical_wasmir.rs
git add .
git commit -qm base
git switch -qc topic
printf '// changed\n' >> crates/witchy-wir/src/canonical_wasmir.rs
git add .
git commit -qm change
export PATH="$tmp/bin:$PATH" MOCK_LOG="$tmp/cargo.jsonl" CARGO_TARGET_DIR="$tmp/target"
export MERGE_QUEUE_STATE_DIR="$tmp/state" CARGO_PROFILE_TEST_STRIP=none
unset WITCHY_GATE_SCOPE WITCHY_ORACLE_EXAMPLE
sha="$(git rev-parse HEAD)"
receipt="$tmp/state/receipts/$sha.json"

run_agent() {
    local status=0
    : > "$MOCK_LOG"
    ./scripts/agent-check.sh edit "$@" > "$tmp/output" 2>&1 || status=$?
    if [ "$status" -ne 0 ]; then cat "$tmp/output" >&2; fi
    return "$status"
}
run_agent
jq -e --arg sha "$sha" '.sha == $sha and (.shards | index("edit") != null)' "$receipt" >/dev/null
python3 - "$MOCK_LOG" <<'PY'
import json, os, pathlib, sys
rows = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
assert len(rows) == 6, rows
assert rows[0]["args"][0] == "metadata"
assert rows[1]["args"][:2] == ["nextest", "list"]
assert rows[1]["args"].count("-p") == 3
assert all(p in rows[1]["args"] for p in ["witchy-wir", "witchy-lower", "witchy", "misc", "cli_subcommands", "rfc0165_route_gate", "codegen"])
for row in rows:
    # This fixture has no live gate lock, so the focused compiler workflow is
    # foreground work. The dedicated agent-check QoS fixture separately pins
    # utility priority while a gate owns the lock.
    assert row["utility"] is None, row
    assert row["target"] == os.environ["CARGO_TARGET_DIR"], row
    assert row["strip"] == "none", row
assert [r["oracle"] for r in rows] == [None, None, None, None, "calc", None]
for row in rows[2:]:
    a = row["args"]
    assert a[:2] == ["nextest", "run"] and "--binaries-metadata" in a and "--cargo-metadata" in a
    assert not any(arg in a for arg in ["--features", "-p", "--lib", "--bin", "--test"]), a
assert "--ignore-default-filter" not in rows[2]["args"]
assert "--ignore-default-filter" not in rows[3]["args"]
assert "--ignore-default-filter" in rows[4]["args"]
expected_scopes = [
    "package(=witchy-wir) and kind(test) and (binary(=unit) or binary(=rfc0165_route_gate))",
    "package(=witchy-lower) and (kind(lib) or (kind(test) and binary(=codegen)))",
    "package(=witchy) and ((kind(bin) and binary(=witchy)) or (kind(test) and binary(=misc)))",
]
for row, scope in zip(rows[2:5], expected_scopes):
    assert row["args"][row["args"].index("-E") + 1].startswith(scope + " and (")
assert "binary(=cli_subcommands)" in rows[5]["args"][rows[5]["args"].index("-E") + 1]
assert "fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact" in rows[5]["args"][rows[5]["args"].index("-E") + 1]
# Metadata belongs only to this run; no stale reusable success cache remains.
assert not pathlib.Path(rows[2]["args"][rows[2]["args"].index("--binaries-metadata") + 1]).exists()
PY

# Compare the exact router expressions with those executed by the fixed helper.
# shlex is used only by this test oracle, never to interpret production commands.
./scripts/test-for-paths.sh --edit-loop > "$tmp/plan"
python3 - "$tmp/plan" "$MOCK_LOG" <<'PY'
import json, pathlib, shlex, sys
commands = [shlex.split(line.strip()) for line in pathlib.Path(sys.argv[1]).read_text().splitlines() if line.startswith("  ")]
rows = [json.loads(line) for line in pathlib.Path(sys.argv[2]).read_text().splitlines()][2:]
assert len(commands) == 3
for command, row in zip(commands, rows[:3]):
    original = command[command.index("-E") + 1]
    actual = row["args"][row["args"].index("-E") + 1]
    assert actual.endswith(" and (" + original + ")"), (original, actual)
PY
rm -f "$receipt"
# An explicit partial selection cannot mint a whole-diff receipt. Keep this on
# an actual native boundary: canonical-interpreter core edits now deliberately
# use the pure WIR route and do not invoke this helper.
run_agent crates/witchy-wir/src/canonical_safewir/build_read.rs
[ ! -e "$receipt" ]
# Dirty input never certifies the committed tip.
printf '// dirty\n' >> crates/witchy-lower/src/canonical_lower.rs
run_agent
[ ! -e "$receipt" ]
git add .
git commit -qm dirty-fixture-change
sha="$(git rev-parse HEAD)"
receipt="$tmp/state/receipts/$sha.json"
for failure in metadata build empty malformed missing-unit missing-rfc0165_route_gate missing-witchy_lower missing-witchy missing-misc wir lower root head-change; do
    if MOCK_FAIL="$failure" run_agent; then
        echo "unexpected success for $failure" >&2
        exit 1
    fi
    [ ! -e "$receipt" ]
    [ ! -e "$tmp/state/receipts/$(git rev-parse HEAD).json" ]
done

# Recipe additions remain selected; standalone runtime-native is NOT absorbed
# into root's fixture-enabled union. Crypto retains the same rule.
for path in crates/witchy-wir/src/canonical_safewir/build_read.rs crates/witchy-wir/src/canonical_interpreter/crypto_rsa_tests.rs; do
    run_agent "$path"
    python3 - "$MOCK_LOG" <<'PY'
import json, pathlib, sys
rows = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
assert len(rows) == 7, rows
assert sum(r["args"][:2] == ["nextest", "list"] for r in rows) == 1
last = rows[-1]["args"]
assert last[:2] == ["nextest", "run"] and "witchy-runtime" in last
assert last[last.index("--features") + 1] == "native"
assert "--binaries-metadata" not in last
assert rows[-1]["oracle"] is None
if "codegen" in rows[1]["args"]:
    assert "binary(=codegen)" in rows[3]["args"][rows[3]["args"].index("-E") + 1]
PY
done
if MOCK_FAIL=missing-codegen run_agent crates/witchy-wir/src/canonical_safewir/build_read.rs; then
    echo "missing recipe binary unexpectedly succeeded" >&2; exit 1
fi
if MOCK_FAIL=runtime run_agent crates/witchy-wir/src/canonical_safewir/build_read.rs; then
    echo "failed standalone runtime unexpectedly succeeded" >&2; exit 1
fi
printf 'canonical-native-edit: one union build, exact scopes/filters, separate runtime profiles, QoS, failures and receipts passed\n'
