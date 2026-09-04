#!/usr/bin/env bash
# RFC-0165 frozen adapter-admitted source census.
#
# This command deliberately does not guess classifications or observations. A
# freeze requires an explicit executor implementing the adapter boundary
# contract below. The normal check is read-only and rejects an unfrozen or
# stale manifest.
#
# Freeze executor contract:
#   WITCHY_SAFEWIR_FREEZE_HOOK=/absolute/path/to/executor \
#     scripts/safewir-census.sh --freeze
#
# The executor is called once per discovered source/profile as:
#   executor --source PATH --profile PROFILE
# Its committed contract is keyed by both PATH and PROFILE. Missing metadata is
# an error; the executor may not classify a source from its name or exit code.
# It must print one JSON object on stdout containing:
#   classification: known-valid | intentionally-invalid | external-fixture
#   adapter_admitted: boolean
#   freeze_class: v1-direct | v1-materialized | v2-required | null
#   input_domain: non-empty object
#   environment_domain: non-empty object
#   observation_contract: non-empty string
#   trace: { status: recorded, version: 1, record: {...}, digest: SHA-256 }
#   for every source/profile. The record is the digest input, not a success
#   marker: it contains every executed route step and its captured effects.
#
# The hook is the missing production adapter/interpreter/SafeWIR execution
# bridge. Until it exists, --freeze refuses to modify the committed manifest.
set -euo pipefail
cd "$(dirname "$0")/.."

exec python3 - "$@" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path.cwd()
MANIFEST = ROOT / "rfcs/0165-safewir-census.json"
SCHEMA = ROOT / "rfcs/0165-safewir-census.schema.json"
SOURCE_ROOTS = ("examples", "projects", "std", "tests")
PROFILES = ("normal", "opt", "native")
CLASSIFICATIONS = ("known-valid", "intentionally-invalid", "external-fixture")
FREEZE_CLASSES = ("v1-direct", "v1-materialized", "v2-required")


def fail(message):
    print(f"safewir-census: ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def digest_bytes(data):
    return hashlib.sha256(data).hexdigest()


def digest_file(path):
    return digest_bytes(path.read_bytes())


def rel(path):
    return path.relative_to(ROOT).as_posix()


def source_files():
    found = []
    for root_name in SOURCE_ROOTS:
        root = ROOT / root_name
        if not root.is_dir():
            continue
        found.extend(path for path in root.rglob("*.witchy") if path.is_file())
    return sorted({rel(path) for path in found})


def nearest_manifest(source):
    path = ROOT / source
    directory = path.parent
    while True:
        candidate = directory / "witchy.toml"
        if candidate.is_file():
            return rel(candidate), digest_file(candidate)
        if directory == ROOT:
            return None, None
        directory = directory.parent


def expected_keys():
    return [(source, profile) for source in source_files() for profile in PROFILES]


def snapshot_digest(entries):
    payload = [
        {key: value for key, value in entry.items() if key != "entry_id"}
        for entry in entries
    ]
    return digest_bytes(canonical(payload).encode("utf-8"))


def read_json(path):
    try:
        with path.open(encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {rel(path)} as JSON: {error}")


def require_object(value, label):
    if not isinstance(value, dict) or not value:
        fail(f"{label} must be a non-empty object")


def validate_entry(entry, index, frozen):
    if not isinstance(entry, dict):
        fail(f"entries[{index}] must be an object")
    required = (
        "entry_id", "source", "profile", "classification", "adapter_admitted",
        "freeze_class", "source_digest", "config_path", "config_digest",
        "input_domain", "environment_domain", "observation_contract", "trace",
    )
    for key in required:
        if key not in entry:
            fail(f"entries[{index}] is missing {key}")
    if not isinstance(entry["entry_id"], str) or not entry["entry_id"]:
        fail(f"entries[{index}].entry_id must be non-empty")
    if entry["source"] not in source_files():
        fail(f"entries[{index}] names undiscovered source {entry['source']}")
    if entry["profile"] not in PROFILES:
        fail(f"entries[{index}] has unsupported profile {entry['profile']}")
    if entry["classification"] not in CLASSIFICATIONS:
        fail(f"entries[{index}] has unsupported classification {entry['classification']}")
    if not isinstance(entry["adapter_admitted"], bool):
        fail(f"entries[{index}].adapter_admitted must be boolean")
    if entry["adapter_admitted"]:
        if entry["freeze_class"] not in FREEZE_CLASSES:
            fail(f"admitted entries[{index}] need a v1/v2 freeze_class")
    elif entry["freeze_class"] is not None:
        fail(f"non-admitted entries[{index}] must have null freeze_class")
    if not isinstance(entry["source_digest"], str) or len(entry["source_digest"]) != 64:
        fail(f"entries[{index}].source_digest must be a SHA-256 hex digest")
    if entry["config_path"] is None:
        if entry["config_digest"] is not None:
            fail(f"entries[{index}] has config_digest without config_path")
    elif not isinstance(entry["config_path"], str) or not isinstance(entry["config_digest"], str):
        fail(f"entries[{index}] needs string config_path/config_digest")
    require_object(entry["input_domain"], f"entries[{index}].input_domain")
    require_object(entry["environment_domain"], f"entries[{index}].environment_domain")
    if not isinstance(entry["observation_contract"], str) or not entry["observation_contract"]:
        fail(f"entries[{index}].observation_contract must be non-empty")
    trace = entry["trace"]
    require_object(trace, f"entries[{index}].trace")
    if trace.get("status") != "recorded" or trace.get("version") != 1:
        fail(f"entries[{index}] need a versioned recorded trace")
    if not isinstance(trace.get("record"), dict) or not trace["record"]:
        fail(f"entries[{index}] need a complete trace record")
    if not isinstance(trace.get("digest"), str) or len(trace["digest"]) != 64:
        fail(f"entries[{index}] need a SHA-256 trace digest")
    actual_trace_digest = digest_bytes(canonical(trace["record"]).encode("utf-8"))
    if trace["digest"] != actual_trace_digest:
        fail(f"entries[{index}] trace digest does not match its record")
    steps = trace["record"].get("steps")
    if not isinstance(steps, list) or not steps or steps[0].get("kind") != "check":
        fail(f"entries[{index}] trace must begin with the ordinary production check")
    check_admitted = steps[0].get("exit_status") == 0
    if entry["adapter_admitted"] != check_admitted:
        fail(f"entries[{index}] adapter_admitted does not match its recorded check result")
    if frozen:
        actual_source_digest = digest_file(ROOT / entry["source"])
        if entry["source_digest"] != actual_source_digest:
            fail(f"source digest changed: {entry['source']}")
        if entry["config_path"] is not None:
            actual_config_digest = digest_file(ROOT / entry["config_path"])
            if entry["config_digest"] != actual_config_digest:
                fail(f"config digest changed: {entry['config_path']}")


def validate_manifest(manifest):
    if not isinstance(manifest, dict):
        fail("manifest root must be an object")
    if manifest.get("schema_version") != 1:
        fail("manifest schema_version must be 1")
    if manifest.get("schema") != rel(SCHEMA):
        fail(f"manifest schema must be {rel(SCHEMA)}")
    if tuple(manifest.get("source_roots", ())) != SOURCE_ROOTS:
        fail("manifest source_roots do not match the fixed RFC-0165 source universe")
    if tuple(manifest.get("profiles", ())) != PROFILES:
        fail("manifest profiles do not match the fixed normal/opt/native matrix")
    if tuple(manifest.get("classifications", ())) != CLASSIFICATIONS:
        fail("manifest classifications are not the explicit RFC-0165 set")
    freeze = manifest.get("freeze")
    if not isinstance(freeze, dict) or freeze.get("state") not in ("pending", "frozen"):
        fail("freeze.state must be pending or frozen")
    entries = manifest.get("entries")
    if not isinstance(entries, list):
        fail("manifest entries must be an array")
    keys = []
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            fail(f"entries[{index}] must be an object")
        if "source" not in entry or "profile" not in entry:
            fail(f"entries[{index}] must have source and profile for sorting")
        keys.append((entry["source"], entry["profile"]))
    if keys != sorted(keys):
        fail("entries must be sorted by source then profile")
    if len(set(keys)) != len(keys):
        fail("entries contain duplicate source/profile keys")
    frozen = freeze["state"] == "frozen"
    for index, entry in enumerate(entries):
        validate_entry(entry, index, frozen)
    expected = expected_keys()
    if frozen and keys != expected:
        missing = sorted(set(expected) - set(keys))
        extra = sorted(set(keys) - set(expected))
        fail(f"frozen source universe mismatch; missing={missing[:3]} extra={extra[:3]}")
    if frozen:
        expected_digest = snapshot_digest(entries)
        if freeze.get("snapshot_digest") != expected_digest:
            fail("freeze.snapshot_digest does not match the committed entries")
        if not freeze.get("adapter_id") or not freeze.get("adapter_commit"):
            fail("frozen manifest needs adapter_id and adapter_commit")
    elif entries:
        fail("pending manifest cannot contain baseline entries")
    return freeze["state"], entries


def run_hook(hook, source, profile):
    try:
        result = subprocess.run(
            [hook, "--source", source, "--profile", profile],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        fail(f"freeze executor failed for {source} [{profile}]: {error}")
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"freeze executor returned invalid JSON for {source} [{profile}]: {error}")
    if not isinstance(payload, dict):
        fail(f"freeze executor result for {source} [{profile}] is not an object")
    return payload


def freeze():
    hook = os.environ.get("WITCHY_SAFEWIR_FREEZE_HOOK")
    adapter_id = os.environ.get("WITCHY_SAFEWIR_ADAPTER_ID")
    adapter_commit = os.environ.get("WITCHY_SAFEWIR_ADAPTER_COMMIT")
    if not hook or not Path(hook).is_file() or not os.access(hook, os.X_OK):
        fail("--freeze requires executable WITCHY_SAFEWIR_FREEZE_HOOK; no trace baseline was fabricated")
    if not adapter_id or not adapter_commit:
        fail("--freeze requires WITCHY_SAFEWIR_ADAPTER_ID and WITCHY_SAFEWIR_ADAPTER_COMMIT")
    current = read_json(MANIFEST)
    state, _ = validate_manifest(current)
    if state == "frozen":
        fail("manifest is already frozen; use a reviewed manifest delta, not an in-place re-freeze")
    generated = []
    for source, profile in expected_keys():
        payload = run_hook(hook, source, profile)
        config_path, config_digest = nearest_manifest(source)
        entry = {
            "entry_id": digest_bytes(f"{source}\0{profile}".encode("utf-8"))[:16],
            "source": source,
            "profile": profile,
            "classification": payload.get("classification"),
            "adapter_admitted": payload.get("adapter_admitted"),
            "freeze_class": payload.get("freeze_class"),
            "source_digest": digest_file(ROOT / source),
            "config_path": config_path,
            "config_digest": config_digest,
            "input_domain": payload.get("input_domain"),
            "environment_domain": payload.get("environment_domain"),
            "observation_contract": payload.get("observation_contract"),
            "trace": payload.get("trace"),
        }
        validate_entry(entry, len(generated), True)
        generated.append(entry)
    replacement = dict(current)
    replacement["freeze"] = {
        "state": "frozen",
        "adapter_route": "ordinary-production-cli",
        "adapter_id": adapter_id,
        "adapter_commit": adapter_commit,
        "snapshot_digest": snapshot_digest(generated),
    }
    replacement["entries"] = generated
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=MANIFEST.parent, delete=False) as handle:
        json.dump(replacement, handle, indent=2, ensure_ascii=True)
        handle.write("\n")
        temporary = Path(handle.name)
    temporary.replace(MANIFEST)
    print(f"safewir-census: froze {len(generated)} source/profile entries")


def check():
    state, entries = validate_manifest(read_json(MANIFEST))
    discovered = len(source_files())
    if state != "frozen":
        fail(f"manifest is pending; discovered {discovered} sources but no adapter baseline exists")
    print(f"safewir-census: checked {len(entries)} frozen source/profile entries ({discovered} sources)")


if len(sys.argv) != 2 or sys.argv[1] not in ("--freeze", "--check"):
    print("usage: scripts/safewir-census.sh --freeze|--check", file=sys.stderr)
    raise SystemExit(2)
if not MANIFEST.is_file() or not SCHEMA.is_file():
    fail("committed census manifest/schema is missing")
if sys.argv[1] == "--freeze":
    freeze()
else:
    check()
PY
