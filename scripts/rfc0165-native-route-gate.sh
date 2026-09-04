#!/usr/bin/env bash
# RFC-0165 native-profile route gate.
#
# The repository's `native` Cargo feature builds the host CLI and its runtime
# dependencies. It is not a third program-codegen backend. Keep this check
# mechanical so the baseline cannot silently record the wasm route as native.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - <<'PY'
from pathlib import Path
import re
import sys

cargo = Path("Cargo.toml").read_text(encoding="utf-8")
cli = Path("src/cli.rs").read_text(encoding="utf-8")

if not re.search(r'^native\s*=\s*\[', cargo, re.MULTILINE):
    print("rfc0165-native-route-gate: ERROR: Cargo feature `native` is missing", file=sys.stderr)
    sys.exit(1)
if 'required-features = ["native"]' not in cargo:
    print(
        "rfc0165-native-route-gate: ERROR: the witchy CLI is no longer explicitly native-feature gated",
        file=sys.stderr,
    )
    sys.exit(1)
if "--backend interpreter|wasm|both" not in cli:
    print(
        "rfc0165-native-route-gate: ERROR: the known program-backend selector changed; review the native route",
        file=sys.stderr,
    )
    sys.exit(1)

print("rfc0165-native-route-gate: unavailable")
print("route_status=missing")
print("cargo_feature=native (host CLI/runtime build feature)")
print("program_backends=interpreter,wasm,both")
print("missing_prerequisite=distinct native program compiler/runtime invocation")
PY
