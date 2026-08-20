#!/usr/bin/env python3
"""Infer which example_tests a changed path can move — without coverage data.

Line-level LLVM maps would be the 10x for compiler edits (which example_tests
execute *this* lowering line). That corpus is optional (`--line-map FILE`).
This script is the cheap half that is always available:

  * Tests already name their inputs: `std/json.witchy`, `include_str!(...)`,
    and `// gate-covers: std/chan.witchy` labels.
  * Filename stems (`example_tests/json.rs` ↔ `std/json.witchy`) are a label.
  * Prelude std modules (`list`, `string`, `dict`, `math`, `option`, `result`,
    `policy`, `show`) are linked into every program, so they fail closed to
    the full example_tests matrix.
  * An unlabeled non-prelude std file also fails closed (unknown coverage).

Usage:
    printf '%s\\n' <paths> | scripts/test-impact.py --example-mods

Stdout lines:
    full                         → run every example_tests::* case
    mod <stem>                   → test(/^example_tests::<stem>::/)
    test <regex>                 → test(/^<regex>/)
"""
from __future__ import annotations

import os
import re
import sys
from pathlib import Path

PRELUDE = frozenset(
    {"list", "string", "dict", "math", "option", "result", "policy", "show"}
)

# Seed labels for modules whose tests do not mention `std/foo.witchy` as a
# path. Source files may also declare `// gate-covers: std/foo.witchy`.
DEFAULT_COVERS = {
    "std/chan.witchy": {"async_channels", "concurrency", "gen_async"},
    "std/task.witchy": {"async_channels", "concurrency", "gen_async"},
    "std/future.witchy": {"async_channels", "concurrency", "gen_async"},
    "std/bytes.witchy": {"bytes"},
    "std/http.witchy": {"network"},
    "std/url.witchy": {"network", "strings"},
    "std/server.witchy": {"network"},
    "std/fs.witchy": {"host_modules"},
    "std/path.witchy": {"host_modules"},
    "std/time.witchy": {"host_modules"},
    "std/exec.witchy": {"host_modules"},
    "std/crypto.witchy": {"crypto", "crypto_jwt_oauth"},
}

STD_SWEEPS = (
    "example_tests::example_sweeps::stdlib_has_no_performance_cliffs",
    "example_tests::example_sweeps::all_std_modules_type_check",
    "example_tests::example_sweeps::linker_auto_resolves_std_imports",
    "example_tests::example_sweeps::dce_drops_unused_stdlib_functions",
)

EXAMPLE_SWEEPS = (
    "example_tests::example_sweeps::every_compilable_example_agrees_on_both_backends",
    "example_tests::example_sweeps::examples_agree_under_rc_floor",
    "example_tests::example_sweeps::every_example_agrees_under_unbox",
    "example_tests::example_sweeps::precompiled_wasm_runs_like_the_source",
    "example_tests::example_sweeps::all_examples_validate_via_cli",
    "example_tests::example_sweeps::all_example_rune_tests_pass",
)

DOC_TEST = "example_tests::example_sweeps::documentation_examples_are_valid"

COVERS_RE = re.compile(r"gate-covers:\s*(.+)$")
STD_PATH_RE = re.compile(r"std/([a-z][a-z0-9_]*)")
WITCHY_FILE_RE = re.compile(r"([a-z][a-z0-9_]*)\.witchy")
EXAMPLES_RE = re.compile(r"examples/[A-Za-z0-9_./-]+")


def repo_root() -> Path:
    here = Path(__file__).resolve().parent.parent
    return here


def load_index(root: Path) -> dict:
    """Map corpus path → example_tests module stems that name it."""
    et = root / "src" / "example_tests"
    std_stems = {p.stem for p in (root / "std").glob("*.witchy")}
    by_std: dict[str, set[str]] = {s: set() for s in std_stems}
    by_example: dict[str, set[str]] = {}
    labels: dict[str, set[str]] = {}

    files = list(et.glob("*.rs")) if et.is_dir() else []
    for path in files:
        mod = path.stem
        text = path.read_text(errors="replace")
        for raw in COVERS_RE.findall(text):
            for tok in raw.split():
                labels.setdefault(tok.strip(), set()).add(mod)
                if tok.startswith("std/") and tok.endswith(".witchy"):
                    by_std.setdefault(Path(tok).stem, set()).add(mod)
        for m in STD_PATH_RE.finditer(text):
            if m.group(1) in std_stems:
                by_std[m.group(1)].add(mod)
        for m in WITCHY_FILE_RE.finditer(text):
            if m.group(1) in std_stems:
                by_std[m.group(1)].add(mod)
        for m in EXAMPLES_RE.finditer(text):
            by_example.setdefault(m.group(0), set()).add(mod)
        # Filename stem is a label: json.rs covers std/json.witchy; crypto_jwt
        # covers std/crypto.witchy and std/jwt.witchy when those modules exist.
        for tok in mod.split("_"):
            if tok in std_stems:
                by_std[tok].add(mod)
        if mod in std_stems:
            by_std[mod].add(mod)

    for path, mods in DEFAULT_COVERS.items():
        labels.setdefault(path, set()).update(mods)
        if path.startswith("std/") and path.endswith(".witchy"):
            by_std.setdefault(Path(path).stem, set()).update(mods)

    return {
        "std": by_std,
        "examples": by_example,
        "labels": labels,
        "std_stems": std_stems,
    }


def classify(paths: list[str], index: dict) -> list[str]:
    std_files = [
        p for p in paths if p.startswith("std/") and p.endswith(".witchy")
    ]
    example_files = [p for p in paths if p.startswith("examples/")]
    book_files = [p for p in paths if p.startswith("book/")]
    doc_md = [
        p
        for p in paths
        if p in ("README.md", "CONTRIBUTING.md")
        or (p.startswith("spec/") and p.endswith(".md") and p != "spec/stdlib.md")
    ]

    out: list[str] = []
    mods: set[str] = set()
    tests: set[str] = set()

    for p in std_files:
        stem = Path(p).stem
        if stem in PRELUDE:
            return ["full"]
        found = set(index["std"].get(stem, ()))
        found |= index["labels"].get(p, set())
        found |= index["labels"].get(f"std/{stem}.witchy", set())
        if not found:
            return ["full"]
        found.discard("example_sweeps")
        mods |= found
        tests.update(STD_SWEEPS)

    for p in example_files:
        mods.add("examples_programs")
        tests.update(EXAMPLE_SWEEPS)
        for key, owners in index["examples"].items():
            if p == key or p.startswith(key.rstrip("/") + "/") or key.startswith(p):
                mods |= owners
        mods |= index["labels"].get(p, set())
        mods.discard("example_sweeps")

    if book_files or doc_md:
        tests.add(DOC_TEST)

    if not mods and not tests:
        return []

    for m in sorted(mods):
        out.append(f"mod {m}")
    for t in sorted(tests):
        out.append(f"test {t}")
    return out


def main(argv: list[str]) -> int:
    if "--example-mods" not in argv:
        print("usage: test-impact.py --example-mods", file=sys.stderr)
        return 2
    root = repo_root()
    paths = [ln.strip() for ln in sys.stdin if ln.strip()]
    index = load_index(root)
    for line in classify(paths, index):
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
