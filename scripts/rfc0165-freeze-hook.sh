#!/usr/bin/env bash
# RFC-0165 adapter-era ordinary production CLI observation hook.
set -euo pipefail

exec python3 - "$@" <<'PY'
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

REPO = Path.cwd()
ROOT = Path(os.environ.get("WITCHY_SAFEWIR_ROOT", REPO))
METADATA = Path(os.environ.get(
    "WITCHY_SAFEWIR_FIXTURE_METADATA",
    REPO / "rfcs/0165-safewir-fixtures.json",
))
PROFILE_OPT = {"normal": "none", "opt": "release"}
ROUTE_LABEL = "witchy-current-adapter-ordinary-production-cli"
KIND_CLASSIFICATION = {
    "runnable-deterministic": "known-valid",
    "library": "known-valid",
    "external": "external-fixture",
    "runnable-nondeterministic": "external-fixture",
    "intentionally-invalid": "intentionally-invalid",
}
FREEZE_CLASSES = {"v1-direct", "v1-materialized", "v2-required"}

def fail(message):
    print(f"rfc0165-freeze-hook: ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)

def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)

def sha256(data):
    return hashlib.sha256(data).hexdigest()

def b64(data):
    return base64.b64encode(data).decode("ascii")

def status_value(result):
    return result.returncode if result.returncode >= 0 else f"signal:{-result.returncode}"

def run_step(kind, binary, argv, env):
    try:
        result = subprocess.run(
            [str(binary), *argv], cwd=ROOT, env=env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
        )
    except OSError as error:
        fail(f"cannot execute production CLI: {error}")
    output = bytes(result.stdout)
    if result.returncode == 0:
        outcome_class = "success"
    elif result.returncode < 0:
        outcome_class = "signal"
    elif kind == "check":
        outcome_class = "check-rejected"
    else:
        outcome_class = "process-nonzero-unclassified"
    return {
        "kind": kind,
        "argv": ["witchy", *argv],
        "exit_status": status_value(result),
        "outcome_class": outcome_class,
        "merged_output_base64": b64(output),
        "merged_output_sha256": sha256(output),
        "observation_scope": {
            "process_stream": "stderr was redirected to stdout before process creation; bytes are recorded in pipe-read order",
            "host_effects": "only effects serialized into the merged process stream are observed",
        },
    }

def load_fixture(source):
    try:
        with METADATA.open(encoding="utf-8") as handle:
            metadata = json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read fixture metadata: {error}")
    if metadata.get("schema_version") != 2 or not isinstance(metadata.get("fixtures"), dict):
        fail("fixture metadata must be schema_version 2 with a fixtures object")
    fixture = metadata["fixtures"].get(source)
    if not isinstance(fixture, dict):
        fail(f"no explicit fixture metadata for {source}")
    required = {"kind", "freeze_class", "input_domain", "environment_domain"}
    missing = sorted(required - fixture.keys())
    if missing:
        fail(f"fixture metadata for {source} is missing {missing}")
    if fixture["kind"] not in KIND_CLASSIFICATION:
        fail(f"fixture metadata for {source} has unsupported kind {fixture['kind']}")
    if fixture["freeze_class"] is not None and fixture["freeze_class"] not in FREEZE_CLASSES:
        fail(f"fixture metadata for {source} has unsupported freeze_class")
    if not isinstance(fixture["input_domain"], dict) or not fixture["input_domain"]:
        fail(f"fixture metadata for {source} needs a non-empty input_domain")
    if not isinstance(fixture["environment_domain"], dict) or not fixture["environment_domain"]:
        fail(f"fixture metadata for {source} needs a non-empty environment_domain")
    return fixture

def binary_path():
    configured = os.environ.get("WITCHY_SAFEWIR_WITCHY_BIN")
    candidates = [Path(configured)] if configured else []
    target = os.environ.get("CARGO_TARGET_DIR")
    if target:
        candidates.append(Path(target) / "debug/witchy")
    candidates.extend([REPO / "target/debug/witchy", REPO / "target/release/witchy"])
    for candidate in candidates:
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate.resolve()
    fail("no current witchy binary; set WITCHY_SAFEWIR_WITCHY_BIN or build with CARGO_TARGET_DIR")

def main():
    if len(sys.argv) != 5 or sys.argv[1] != "--source" or sys.argv[3] != "--profile":
        fail("usage: scripts/rfc0165-freeze-hook.sh --source PATH --profile normal|opt|native")
    source, profile = sys.argv[2], sys.argv[4]
    if profile == "native":
        fail("native profile has no distinct native-feature/runtime route; refusing to record the release route as native")
    if profile not in PROFILE_OPT:
        fail(f"unsupported profile {profile}")
    path = ROOT / source
    if not path.is_file() or path.suffix != ".witchy":
        fail(f"source is not a .witchy file under the fixture root: {source}")
    source_bytes = path.read_bytes()
    fixture = load_fixture(source)
    binary = binary_path()
    env = os.environ.copy()
    env["WITCHY_OPT"] = PROFILE_OPT[profile]
    env.pop("WITCHY_SAFEWIR_FREEZE_HOOK", None)
    actual_environment = {
        "profile": profile,
        "witchy_opt": PROFILE_OPT[profile],
        "cwd": ".",
        "route_label": ROUTE_LABEL,
        "binary_sha256": sha256(binary.read_bytes()),
    }
    steps = [run_step("check", binary, ["check", source], env)]
    adapter_admitted = steps[0]["exit_status"] == 0
    if fixture["kind"] == "intentionally-invalid" and adapter_admitted:
        fail(f"intentionally-invalid fixture was admitted by production check: {source} [{profile}]")
    if fixture["kind"] in {"runnable-deterministic", "library", "runnable-nondeterministic"} and not adapter_admitted:
        fail(f"{fixture['kind']} fixture was rejected by production check: {source} [{profile}]")
    freeze_class = fixture["freeze_class"] if adapter_admitted else None
    if adapter_admitted and freeze_class is None:
        fail(f"admitted fixture has no explicit freeze_class: {source} [{profile}]")
    runtime_observed = fixture["kind"] == "runnable-deterministic" and adapter_admitted
    if runtime_observed:
        steps.append(run_step("run", binary, [source], env))
    input_domain = {
        "declared": fixture["input_domain"],
        "source": source,
        "source_sha256": sha256(source_bytes),
    }
    environment_domain = {
        "declared": fixture["environment_domain"],
        "actual": actual_environment,
    }
    record = {
        "version": 1,
        "route": ROUTE_LABEL,
        "source": source,
        "profile": profile,
        "fixture_kind": fixture["kind"],
        "input_domain": input_domain,
        "environment_domain": environment_domain,
        "runtime_observation": "merged-process-stream-recorded" if runtime_observed else "not-run",
        "steps": steps,
        "observation_scope": {
            "covered": "check status and merged process bytes; deterministic runnable fixtures also cover ordinary CLI run status and merged process bytes",
            "not_covered": "filesystem, network, clock, random, scheduling, or other host effects not serialized into the merged process stream",
        },
    }
    trace_digest = sha256(canonical(record).encode("utf-8"))
    print(json.dumps({
        "classification": KIND_CLASSIFICATION[fixture["kind"]],
        "adapter_admitted": adapter_admitted,
        "freeze_class": freeze_class,
        "input_domain": input_domain,
        "environment_domain": environment_domain,
        "observation_contract": "rfc0165-runtime-observation-v1" if runtime_observed else "rfc0165-check-admission-v1",
        "trace": {"status": "recorded", "version": 1, "record": record, "digest": trace_digest},
    }, sort_keys=True, separators=(",", ":")))

main()
PY
