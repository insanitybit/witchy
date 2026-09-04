#!/usr/bin/env bash
# RFC-0165 A2 production-route gate.
#
# This is a source and Cargo-boundary check, not a claim based on comments or
# function names. The boundaries are the private canonical backend module,
# the raw encoder module, and the lowerer's production source tree.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
if [ "$#" -eq 0 ]; then
    :
elif [ "$#" -eq 2 ] && [ "$1" = "--root" ]; then
    root="$(cd "$2" && pwd)"
else
    printf '%s\n' 'usage: scripts/rfc0165-route-gate.sh [--root PATH]' >&2
    exit 2
fi

exec python3 - "$root" <<'PY'
from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(sys.argv[1])
WIR_SRC = ROOT / "crates" / "witchy-wir" / "src"
CANONICAL = WIR_SRC / "canonical_wasmir.rs"
WIR_LIB = WIR_SRC / "lib.rs"
RAW_ENCODER = WIR_SRC / "wir_encode.rs"
LOWER_SRC = ROOT / "crates" / "witchy-lower" / "src"
FORBIDDEN_PRODUCTION_FEATURES = {"legacy-encoder-bridge"}
RAW_STRING_START = re.compile(r"(?:br|r)(#+)\"")


def fail(violations: list[str]) -> None:
    print("rfc0165-route-gate: REJECTED", file=sys.stderr)
    for violation in violations:
        print(f"- {violation}", file=sys.stderr)
    raise SystemExit(1)


def mask_rust(source: str) -> str:
    """Blank comments and string literals while preserving positions/newlines."""
    out = list(source)
    i = 0
    block_depth = 0
    state = "normal"
    while i < len(source):
        if state == "line":
            if source[i] == "\n":
                state = "normal"
            else:
                out[i] = " "
            i += 1
            continue
        if state == "block":
            if source.startswith("/*", i):
                out[i:i + 2] = "  "
                block_depth += 1
                i += 2
            elif source.startswith("*/", i):
                out[i:i + 2] = "  "
                block_depth -= 1
                i += 2
                if block_depth == 0:
                    state = "normal"
            else:
                if source[i] != "\n":
                    out[i] = " "
                i += 1
            continue
        if source.startswith("//", i):
            out[i:i + 2] = "  "
            state = "line"
            i += 2
            continue
        if source.startswith("/*", i):
            out[i:i + 2] = "  "
            state = "block"
            block_depth = 1
            i += 2
            continue
        raw = RAW_STRING_START.match(source, i) if source[i] in {"b", "r"} else None
        if raw:
            hashes = raw.group(1)
            end = "\"" + hashes + ""
            j = i + raw.end()
            while j < len(source) and not source.startswith(end, j):
                if source[j] != "\n":
                    out[j] = " "
                j += 1
            j = min(len(source), j + len(end))
            for k in range(i, j):
                if source[k] != "\n":
                    out[k] = " "
            i = j
            continue
        if source[i] == '"':
            j = i + 1
            escaped = False
            while j < len(source):
                char = source[j]
                if char == "\n":
                    break
                if char == '"' and not escaped:
                    j += 1
                    break
                escaped = char == "\\" and not escaped
                if char != "\\":
                    escaped = False
                j += 1
            for k in range(i, j):
                if source[k] != "\n":
                    out[k] = " "
            i = j
            continue
        i += 1
    return "".join(out)


def rust_files(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    return sorted(path for path in directory.rglob("*.rs") if path.is_file())


def line_number(source: str, offset: int) -> int:
    return source.count("\n", 0, offset) + 1


def matching(masked: str, opening: int, left: str = "(", right: str = ")") -> int | None:
    depth = 0
    for index in range(opening, len(masked)):
        if masked[index] == left:
            depth += 1
        elif masked[index] == right:
            depth -= 1
            if depth == 0:
                return index
    return None


def matching_pairs(masked: str, left: str, right: str) -> dict[int, int]:
    stack: list[int] = []
    pairs: dict[int, int] = {}
    for index, char in enumerate(masked):
        if char == left:
            stack.append(index)
        elif char == right and stack:
            pairs[stack.pop()] = index
    return pairs


def function_records(source: str, masked: str | None = None) -> list[dict[str, object]]:
    masked = mask_rust(source) if masked is None else masked
    parentheses = matching_pairs(masked, "(", ")")
    braces = matching_pairs(masked, "{", "}")
    records = []
    pattern = re.compile(
        r"(?m)^[ \t]*(?P<visibility>pub(?:\s*\([^\n)]*\))?\s+)?"
        r"(?:(?:async|const|unsafe)\s+)*fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*\("
    )
    for match in pattern.finditer(masked):
        opening = masked.find("(", match.start(), match.end())
        closing = parentheses.get(opening)
        if closing is None:
            continue
        signature_end = closing + 1
        brace = masked.find("{", signature_end)
        semicolon = masked.find(";", signature_end)
        if brace < 0 or (semicolon >= 0 and semicolon < brace):
            continue
        end = braces.get(brace)
        if end is None:
            continue
        line_start = masked.rfind("\n", 0, match.start()) + 1
        # Only the contiguous attribute block immediately preceding this
        # function applies to it. Looking back an arbitrary byte window can
        # accidentally borrow a neighboring function's cfg authority.
        attribute_match = re.search(
            r"(?ms)((?:^[ \t]*#\[[^\]]*\][ \t]*\n)+)[ \t]*$",
            source[:line_start],
        )
        attribute_lines = attribute_match.group(1) if attribute_match else ""
        records.append({
            "name": match.group("name"),
            "visibility": match.group("visibility") or "",
            "signature": masked[match.start():signature_end],
            "attributes": attribute_lines,
            "start": match.start(),
            "brace": brace,
            "end": end,
        })
    return records


def dependency_tables(value: object, path: tuple[str, ...] = ()):
    if not isinstance(value, dict):
        return
    for key, child in value.items():
        if key in {"dependencies", "build-dependencies", "dev-dependencies"} and isinstance(child, dict):
            yield path + (key,), child
        elif key == "target":
            yield from dependency_tables(child, path + (key,))


def is_raw_feature(name: str) -> bool:
    return "raw" in name or "test-api" in name


def check_features(violations: list[str]) -> None:
    manifests = [ROOT / "Cargo.toml"] + sorted((ROOT / "crates").glob("*/Cargo.toml"))
    manifests = [path for path in manifests if path.is_file()]
    parsed = []
    raw_features: set[str] = set()
    for path in manifests:
        try:
            data = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as error:
            violations.append(f"cannot parse Cargo manifest {path.relative_to(ROOT)}: {error}")
            continue
        parsed.append((path, data))
        for feature in data.get("features", {}):
            if is_raw_feature(feature):
                raw_features.add(feature)
    for path, data in parsed:
        for section_path, dependencies in dependency_tables(data):
            section = section_path[-1]
            development_only = section == "dev-dependencies"
            for dependency, spec in dependencies.items():
                if not isinstance(spec, dict):
                    continue
                for feature in spec.get("features", []):
                    if feature in FORBIDDEN_PRODUCTION_FEATURES or (
                        feature in raw_features and not development_only
                    ):
                        if feature in FORBIDDEN_PRODUCTION_FEATURES or not development_only:
                            violations.append(
                                f"forbidden dependency edge enables `{feature}`: "
                                f"{path.relative_to(ROOT)} [{section}] {dependency}"
                            )


def check_canonical_backend(violations: list[str]) -> None:
    if not WIR_LIB.is_file() or not CANONICAL.is_file():
        violations.append("canonical WasmIR module or library declaration is missing")
        return
    lib = mask_rust(WIR_LIB.read_text(encoding="utf-8"))
    if not re.search(r"(?m)^\s*mod\s+canonical_wasmir\s*;", lib):
        violations.append("canonical_wasmir is not declared as a private module")
    if re.search(r"(?m)^\s*pub\s+mod\s+canonical_wasmir\s*;", lib):
        violations.append("canonical_wasmir is publicly declared")

    sources = rust_files(WIR_SRC)
    canonical_source = CANONICAL.read_text(encoding="utf-8")
    canonical_masked = mask_rust(canonical_source)
    wasm_ir_occurrences = []
    for path in sources:
        source = path.read_text(encoding="utf-8")
        masked = mask_rust(source)
        for match in re.finditer(r"\bWasmIr\b", masked):
            wasm_ir_occurrences.append((path, match.start()))
    outside = [(path, offset) for path, offset in wasm_ir_occurrences if path != CANONICAL]
    for path, offset in outside:
        violations.append(
            f"private WasmIr appears outside canonical backend: "
            f"{path.relative_to(ROOT)}:{line_number(path.read_text(encoding='utf-8'), offset)}"
        )

    declarations = list(re.finditer(r"\bstruct\s+WasmIr\b", canonical_masked))
    if len(declarations) != 1:
        violations.append(f"canonical backend has {len(declarations)} WasmIr declarations; expected one")
    if declarations and re.search(r"\bpub\b", canonical_masked[canonical_masked.rfind("\n", 0, declarations[0].start()) + 1:declarations[0].start()]):
        violations.append("WasmIr declaration is public")

    declaration_spans = [
        match.span() for match in re.finditer(r"\bstruct\s+WasmIr\s*\{", canonical_masked)
    ]
    constructions = [
        match for match in re.finditer(r"\bWasmIr\s*\{", canonical_masked)
        if not any(start <= match.start() < end for start, end in declaration_spans)
        and not re.search(r"->\s*$", canonical_masked[max(0, match.start() - 32):match.start()])
    ]
    if len(constructions) != 1:
        violations.append(f"canonical backend has {len(constructions)} WasmIr construction sites; expected one")
        return
    position = constructions[0].start()
    containing = [record for record in function_records(canonical_source, canonical_masked)
                  if int(record["brace"]) < position < int(record["end"])]
    if len(containing) != 1:
        violations.append("WasmIr construction is not inside exactly one function")
        return
    function = containing[0]
    if "ValidatedSafeWir" not in str(function["signature"]):
        violations.append("WasmIr construction function does not take ValidatedSafeWir")
    prefix = canonical_masked[canonical_masked.rfind("\n", 0, int(function["start"])) + 1:int(function["start"])]
    if re.search(r"\bpub\b", prefix):
        violations.append("WasmIr construction function is public")


def check_raw_encoder(violations: list[str]) -> list[str]:
    if not RAW_ENCODER.is_file():
        violations.append("raw encoder module is missing")
        return []
    source = RAW_ENCODER.read_text(encoding="utf-8")
    raw_names = []
    for function in function_records(source):
        signature = str(function["signature"])
        if "ValidatedWirModule" in signature or not re.search(r"\bWirModule\b", signature):
            continue
        attributes = str(function["attributes"])
        externally_visible = str(function["visibility"]).startswith("pub") and not str(function["visibility"]).startswith("pub(crate")
        if externally_visible:
            if "cfg" not in attributes or not re.search(r"\btest\b|raw-encoder-test-api", attributes):
                violations.append(f"WirModule encoder surface is not test-gated: {function['name']}")
            if "legacy-encoder-bridge" in attributes:
                violations.append(f"production legacy encoder bridge remains reachable: {function['name']}")
            raw_names.append(str(function["name"]))
    return raw_names


def check_lowerer_calls(raw_names: list[str], violations: list[str]) -> None:
    if not LOWER_SRC.is_dir():
        violations.append("lowerer production source boundary is missing")
        return
    for path in rust_files(LOWER_SRC):
        source = path.read_text(encoding="utf-8")
        masked = mask_rust(source)
        functions = function_records(source, masked)
        for name in raw_names:
            for call in re.finditer(rf"\bwir_encode\s*::\s*{re.escape(name)}\s*\(", masked):
                containing = [
                    function
                    for function in functions
                    if int(function["brace"]) < call.start() < int(function["end"])
                ]
                if len(containing) == 1:
                    attributes = str(containing[0]["attributes"])
                    if "cfg" in attributes and re.search(
                        r"\btest\b|raw-module-test-api", attributes
                    ):
                        continue
                violations.append(
                    f"lowerer production source invokes raw encoder `{name}`: {path.relative_to(ROOT)}"
                )


violations: list[str] = []
check_features(violations)
check_canonical_backend(violations)
raw_names = check_raw_encoder(violations)
check_lowerer_calls(raw_names, violations)
if violations:
    fail(violations)

print("rfc0165-route-gate: accepted")
print("canonical_backend=crates/witchy-wir/src/canonical_wasmir.rs")
print("wasmir_construction=private function taking ValidatedSafeWir")
print("raw_encoder_surfaces=test-or-raw-encoder-test-api only")
print("production_raw_encoder_calls=0")
print("production_raw_feature_edges=0")
PY
