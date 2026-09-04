#!/usr/bin/env bash
# RFC-0165 A10 frozen source-universe and observation-manifest gate.
#
# This gate is deliberately independent of the compiler. It checks that a
# committed freeze is complete for the declared source roots and profiles,
# that source/configuration identities have not changed, and that every
# recorded observation authenticates its own trace. It never invents a result.
set -euo pipefail

exec python3 - "$@" <<'PY'
import argparse
import hashlib
import json
from pathlib import Path
import sys


PROFILES = ("normal", "opt", "native")
CLASSIFICATIONS = ("known-valid", "intentionally-invalid", "external-fixture")
FREEZE_CLASSES = ("v1-direct", "v1-materialized", "v2-required")
OBSERVATION_STATUSES = ("recorded", "unavailable")


def fail(message):
    print(f"rfc0165-freeze-manifest: ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def digest_bytes(value):
    return hashlib.sha256(value).hexdigest()


def digest_json(value):
    return digest_bytes(canonical(value).encode("utf-8"))


def load_json(path, label):
    try:
        with path.open(encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {label}: {error}")


def source_universe(root, roots, extensions):
    found = set()
    for name in roots:
        directory = root / name
        if not directory.is_dir():
            continue
        for extension in extensions:
            found.update(
                path.relative_to(root).as_posix()
                for path in directory.rglob(f"*{extension}")
                if path.is_file()
            )
    return sorted(found)


def require_string(value, label):
    if not isinstance(value, str) or not value:
        fail(f"{label} must be a non-empty string")


def require_digest(value, label):
    if not isinstance(value, str) or len(value) != 64 or any(c not in "0123456789abcdef" for c in value):
        fail(f"{label} must be a lowercase SHA-256 digest")


def require_object(value, label):
    if not isinstance(value, dict) or not value:
        fail(f"{label} must be a non-empty object")


def validate_observation(observation, source, profile):
    require_object(observation, f"{source} [{profile}].observation")
    status = observation.get("status")
    if status not in OBSERVATION_STATUSES:
        fail(f"{source} [{profile}] has unsupported observation status")
    if status == "unavailable":
        require_string(observation.get("reason"), f"{source} [{profile}].observation.reason")
        if set(observation) != {"status", "reason"}:
            fail(f"{source} [{profile}] unavailable observation has extra guessed data")
        return
    if set(observation) != {"status", "version", "record", "digest"}:
        fail(f"{source} [{profile}] recorded observation has incomplete/extra fields")
    if observation["version"] != 1:
        fail(f"{source} [{profile}] observation version must be 1")
    require_object(observation["record"], f"{source} [{profile}].observation.record")
    if observation["record"].get("source") != source or observation["record"].get("profile") != profile:
        fail(f"{source} [{profile}] observation identity does not match its entry")
    steps = observation["record"].get("steps")
    if not isinstance(steps, list) or not steps:
        fail(f"{source} [{profile}] recorded observation has no execution steps")
    for index, step in enumerate(steps):
        if not isinstance(step, dict) or not step.get("kind") or "exit_status" not in step:
            fail(f"{source} [{profile}] observation step {index} is incomplete")
    require_digest(observation["digest"], f"{source} [{profile}].observation.digest")
    if observation["digest"] != digest_json(observation["record"]):
        fail(f"{source} [{profile}] observation digest does not match its record")


def validate_profile(profile_value, source, profile_name, frozen):
    if not isinstance(profile_value, dict):
        fail(f"{source} [{profile_name}] must be an object")
    required = {
        "classification", "adapter_admitted", "freeze_class", "input_domain",
        "environment_domain", "observation_contract", "observation",
    }
    if set(profile_value) != required:
        missing = sorted(required - set(profile_value))
        extra = sorted(set(profile_value) - required)
        fail(f"{source} [{profile_name}] schema mismatch missing={missing} extra={extra}")
    if profile_value["classification"] not in CLASSIFICATIONS:
        fail(f"{source} [{profile_name}] has unsupported classification")
    if not isinstance(profile_value["adapter_admitted"], bool):
        fail(f"{source} [{profile_name}].adapter_admitted must be boolean")
    if profile_value["freeze_class"] is not None and profile_value["freeze_class"] not in FREEZE_CLASSES:
        fail(f"{source} [{profile_name}] has unsupported freeze_class")
    if profile_value["adapter_admitted"] != (profile_value["freeze_class"] is not None):
        fail(f"{source} [{profile_name}] freeze_class disagrees with adapter admission")
    if profile_value["classification"] == "intentionally-invalid" and profile_value["adapter_admitted"]:
        fail(f"{source} [{profile_name}] invalid classification was admitted")
    require_object(profile_value["input_domain"], f"{source} [{profile_name}].input_domain")
    require_object(profile_value["environment_domain"], f"{source} [{profile_name}].environment_domain")
    require_string(profile_value["observation_contract"], f"{source} [{profile_name}].observation_contract")
    if frozen:
        validate_observation(profile_value["observation"], source, profile_name)
    elif profile_value["observation"] is not None:
        fail(f"pending manifest cannot contain an observation for {source} [{profile_name}]")


def entry_payload(entry):
    return {key: value for key, value in entry.items() if key != "entry_digest"}


def validate(manifest, root):
    if not isinstance(manifest, dict):
        fail("manifest root must be an object")
    if manifest.get("schema_version") != 1:
        fail("schema_version must be 1")
    if manifest.get("state") not in ("pending", "frozen"):
        fail("state must be pending or frozen")
    frozen = manifest["state"] == "frozen"
    roots = manifest.get("source_roots")
    extensions = manifest.get("source_extensions")
    profiles = manifest.get("profiles")
    if not isinstance(roots, list) or roots != sorted(set(roots)) or not roots:
        fail("source_roots must be a sorted non-empty list")
    if not isinstance(extensions, list) or extensions != sorted(set(extensions)) or not extensions:
        fail("source_extensions must be a sorted non-empty list")
    if profiles != list(PROFILES):
        fail("profiles must be exactly [normal, opt, native]")
    entries = manifest.get("entries")
    if not isinstance(entries, list):
        fail("entries must be an array")
    keys = []
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            fail(f"entries[{index}] must be an object")
        if set(entry) != {"source", "source_sha256", "profiles", "entry_digest"}:
            fail(f"entries[{index}] has incomplete/extra fields")
        source = entry["source"]
        require_string(source, f"entries[{index}].source")
        if Path(source).is_absolute() or ".." in Path(source).parts:
            fail(f"entries[{index}] source escapes the manifest root")
        keys.append(source)
        require_digest(entry["source_sha256"], f"entries[{index}].source_sha256")
        if not isinstance(entry["profiles"], dict) or list(entry["profiles"]) != list(PROFILES):
            fail(f"{source} profiles must be explicit and ordered normal/opt/native")
        for profile in PROFILES:
            validate_profile(entry["profiles"][profile], source, profile, frozen)
        require_digest(entry["entry_digest"], f"{source}.entry_digest")
        if entry["entry_digest"] != digest_json(entry_payload(entry)):
            fail(f"{source} entry digest does not match its contents")
    if keys != sorted(keys):
        fail("entries must be sorted by source")
    if len(keys) != len(set(keys)):
        fail("entries contain duplicate sources")
    discovered = source_universe(root, roots, extensions)
    if keys != discovered:
        missing = sorted(set(discovered) - set(keys))
        extra = sorted(set(keys) - set(discovered))
        fail(f"source universe mismatch; missing={missing[:5]} extra={extra[:5]}")
    actual_entries = []
    for entry in entries:
        source_path = root / entry["source"]
        if not source_path.is_file():
            fail(f"manifest source is not a file: {entry['source']}")
        actual = digest_bytes(source_path.read_bytes())
        if actual != entry["source_sha256"]:
            fail(f"source digest changed: {entry['source']}")
        actual_entries.append(entry)
    universe_digest = digest_json(discovered)
    if manifest.get("universe_digest") != universe_digest:
        fail(f"universe_digest does not match the discovered sorted source universe: expected {universe_digest}")
    snapshot_digest = digest_json(actual_entries)
    if manifest.get("snapshot_digest") != snapshot_digest:
        fail("snapshot_digest does not match the committed entries")
    return manifest["state"], universe_digest, snapshot_digest


def main():
    parser = argparse.ArgumentParser(description="check an RFC-0165 A10 freeze manifest")
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--expected-snapshot-digest")
    args = parser.parse_args()
    manifest = load_json(args.manifest, "manifest")
    state, universe_digest, snapshot_digest = validate(manifest, args.root)
    if args.expected_snapshot_digest and args.expected_snapshot_digest != snapshot_digest:
        fail("snapshot digest differs from the trusted expected digest")
    print(json.dumps({"state": state, "sources": len(manifest["entries"]),
                      "universe_digest": universe_digest, "snapshot_digest": snapshot_digest},
                     sort_keys=True, separators=(",", ":")))


main()
PY
