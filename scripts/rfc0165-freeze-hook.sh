#!/usr/bin/env bash
# RFC-0165 ordinary production CLI observation hook.
#
# The hook is intentionally a recorder, not a classifier. Its committed
# contract is keyed by source and profile and supplies classification,
# freeze_class, domains, observation scope, and whether execution is required.
# Admission is the only fact derived from execution: it is the exit status of
# the first ordinary production `witchy check PATH` step.
#
# An ordinary CLI may optionally emit this line on stdout or stderr:
#   WITCHY_SAFEWIR_OBSERVATION_V1={"trap_class":...,"resource_counters":...,"in_place_counters":...}
# Only fields actually emitted are recorded. No unavailable observation is
# synthesized.
set -euo pipefail

exec python3 - "$@" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

REPO = Path.cwd()
ROOT = Path(os.environ.get("WITCHY_SAFEWIR_ROOT", REPO))
METADATA = Path(os.environ.get(
    "WITCHY_SAFEWIR_FIXTURE_METADATA",
    REPO / "rfcs/0165-safewir-fixtures.json",
))
PROFILE_OPT = {"normal": "none", "opt": "release", "native": "release"}
PROFILES = tuple(PROFILE_OPT)
CLASSIFICATIONS = {"known-valid", "intentionally-invalid", "external-fixture"}
FREEZE_CLASSES = {"v1-direct", "v1-materialized", "v2-required"}
EXECUTION_MODES = {"check-only", "check-then-run"}
CHECK_EXPECTATIONS = {"admitted", "rejected", "either"}
OBSERVATION_MARKER = b"WITCHY_SAFEWIR_OBSERVATION_V1="


def fail(message):
    print(f"rfc0165-freeze-hook: ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def status_value(result):
    return result.returncode if result.returncode >= 0 else f"signal:{-result.returncode}"


def require_string(value, label):
    if not isinstance(value, str) or not value:
        fail(f"{label} must be a non-empty string")


def require_object(value, label):
    if not isinstance(value, dict) or not value:
        fail(f"{label} must be a non-empty object")


def load_contract(source, profile):
    try:
        with METADATA.open(encoding="utf-8") as handle:
            metadata = json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read fixture contract: {error}")
    if metadata.get("schema_version") != 3 or not isinstance(metadata.get("contracts"), dict):
        fail("fixture contract must be schema_version 3 with a contracts object")
    source_contract = metadata["contracts"].get(source)
    if not isinstance(source_contract, dict) or not isinstance(source_contract.get("profiles"), dict):
        fail(f"no explicit source/profile contract for {source} [{profile}]")
    contract = source_contract["profiles"].get(profile)
    if not isinstance(contract, dict):
        fail(f"no explicit source/profile contract for {source} [{profile}]")
    required = {
        "classification", "freeze_class", "check_expectation", "execution",
        "input_domain", "environment_domain", "observation_contract",
    }
    missing = sorted(required - contract.keys())
    if missing:
        fail(f"contract for {source} [{profile}] is missing {missing}")
    if contract["classification"] not in CLASSIFICATIONS:
        fail(f"contract for {source} [{profile}] has unsupported classification")
    if contract["freeze_class"] is not None and contract["freeze_class"] not in FREEZE_CLASSES:
        fail(f"contract for {source} [{profile}] has unsupported freeze_class")
    if contract["check_expectation"] not in CHECK_EXPECTATIONS:
        fail(f"contract for {source} [{profile}] has unsupported check_expectation")
    if contract["execution"] not in EXECUTION_MODES:
        fail(f"contract for {source} [{profile}] has unsupported execution mode")
    require_object(contract["input_domain"], f"contract for {source} [{profile}].input_domain")
    require_object(contract["environment_domain"], f"contract for {source} [{profile}].environment_domain")
    require_string(contract["observation_contract"], f"contract for {source} [{profile}].observation_contract")
    normalization = contract.get("normalization", {})
    if not isinstance(normalization, dict):
        fail(f"contract for {source} [{profile}].normalization must be an object")
    for stream in normalization:
        if stream not in {"stdout", "stderr"}:
            fail(f"contract for {source} [{profile}] normalizes unknown stream {stream}")
        if not isinstance(normalization[stream], list):
            fail(f"contract for {source} [{profile}].normalization.{stream} must be an array")
        for rule in normalization[stream]:
            if not isinstance(rule, dict) or set(rule) != {"pattern", "replacement"}:
                fail(f"contract for {source} [{profile}] has malformed normalization rule")
            require_string(rule["pattern"], "normalization pattern")
            if not isinstance(rule["replacement"], str):
                fail("normalization replacement must be a string")
            try:
                re.compile(rule["pattern"].encode("utf-8"))
            except re.error as error:
                fail(f"invalid normalization pattern: {error}")
    return contract


def normalize(data, rules):
    for rule in rules:
        data = re.sub(
            rule["pattern"].encode("utf-8"),
            rule["replacement"].encode("utf-8"),
            data,
        )
    return data


def marker_observations(stdout, stderr):
    observations = {}
    for stream in (stdout, stderr):
        for line in stream.splitlines():
            if not line.startswith(OBSERVATION_MARKER):
                continue
            try:
                value = json.loads(line[len(OBSERVATION_MARKER):])
            except json.JSONDecodeError as error:
                fail(f"production CLI emitted invalid observation marker: {error}")
            if not isinstance(value, dict):
                fail("production CLI observation marker must contain an object")
            for key in value:
                if key not in {"trap_class", "resource_counters", "in_place_counters"}:
                    fail(f"production CLI emitted unsupported observation field {key}")
            if "trap_class" in value:
                require_string(value["trap_class"], "trap_class")
            for key in ("resource_counters", "in_place_counters"):
                if key in value:
                    require_object(value[key], key)
            if observations:
                fail("production CLI emitted duplicate observation markers")
            observations = value
    return observations


def run_step(kind, binary, argv, env, normalization):
    try:
        result = subprocess.run(
            [str(binary), *argv], cwd=ROOT, env=env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        )
    except OSError as error:
        fail(f"cannot execute ordinary production CLI: {error}")
    stdout = normalize(bytes(result.stdout), normalization.get("stdout", []))
    stderr = normalize(bytes(result.stderr), normalization.get("stderr", []))
    step = {
        "kind": kind,
        "command_route": "ordinary-production-cli",
        "argv": ["witchy", *argv],
        "exit_status": status_value(result),
        "stdout_bytes": len(stdout),
        "stdout_sha256": sha256(stdout),
        "stderr_bytes": len(stderr),
        "stderr_sha256": sha256(stderr),
    }
    observations = marker_observations(bytes(result.stdout), bytes(result.stderr))
    if observations:
        step["observations"] = observations
    if normalization:
        step["normalization_scope"] = normalization
    return step


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
    fail("no current witchy binary; set WITCHY_SAFEWIR_WITCHY_BIN or CARGO_TARGET_DIR")


def reject_native_profile():
    gate = REPO / "scripts/rfc0165-native-route-gate.sh"
    if not gate.is_file() or not os.access(gate, os.X_OK):
        fail("native profile has no audited distinct production route")
    try:
        result = subprocess.run(
            [str(gate)], cwd=REPO, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, check=False,
        )
    except OSError as error:
        fail(f"cannot execute native route audit: {error}")
    if result.returncode != 0:
        fail(f"native route audit failed: {result.stdout.strip()}")
    fail("native profile has no audited distinct production route")


def main():
    if len(sys.argv) != 5 or sys.argv[1] != "--source" or sys.argv[3] != "--profile":
        fail("usage: scripts/rfc0165-freeze-hook.sh --source PATH --profile normal|opt|native")
    source, profile = sys.argv[2], sys.argv[4]
    if profile not in PROFILES:
        fail(f"unsupported profile {profile}")
    if profile == "native":
        reject_native_profile()
    path = ROOT / source
    if not path.is_file() or path.suffix != ".witchy":
        fail(f"source is not a .witchy file under the fixture root: {source}")
    contract = load_contract(source, profile)
    binary = binary_path()
    env = os.environ.copy()
    env["WITCHY_OPT"] = PROFILE_OPT[profile]
    env.pop("WITCHY_SAFEWIR_FREEZE_HOOK", None)
    normalization = contract.get("normalization", {})
    steps = [run_step("check", binary, ["check", source], env, normalization)]
    adapter_admitted = steps[0]["exit_status"] == 0
    expectation = contract["check_expectation"]
    if expectation == "admitted" and not adapter_admitted:
        fail(f"contract expected check admission: {source} [{profile}]")
    if expectation == "rejected" and adapter_admitted:
        fail(f"contract expected check rejection: {source} [{profile}]")
    if adapter_admitted and contract["freeze_class"] is None:
        fail(f"admitted source/profile contract needs an explicit freeze_class: {source} [{profile}]")
    if not adapter_admitted and contract["freeze_class"] is not None:
        fail(f"rejected source/profile contract must use freeze_class null: {source} [{profile}]")
    if contract["execution"] == "check-then-run" and adapter_admitted:
        steps.append(run_step("run", binary, [source], env, normalization))
    source_bytes = path.read_bytes()
    input_domain = dict(contract["input_domain"])
    input_domain["source"] = source
    input_domain["source_sha256"] = sha256(source_bytes)
    environment_domain = dict(contract["environment_domain"])
    environment_domain["profile"] = profile
    environment_domain["witchy_opt"] = PROFILE_OPT[profile]
    environment_domain["binary_sha256"] = sha256(binary.read_bytes())
    record = {
        "version": 1,
        "route": "ordinary-production-cli",
        "source": source,
        "profile": profile,
        "input_domain": input_domain,
        "environment_domain": environment_domain,
        "steps": steps,
    }
    trace_digest = sha256(canonical(record).encode("utf-8"))
    print(json.dumps({
        "classification": contract["classification"],
        "adapter_admitted": adapter_admitted,
        "freeze_class": contract["freeze_class"],
        "input_domain": input_domain,
        "environment_domain": environment_domain,
        "observation_contract": contract["observation_contract"],
        "trace": {"status": "recorded", "version": 1, "record": record, "digest": trace_digest},
    }, sort_keys=True, separators=(",", ":")))


main()
PY
