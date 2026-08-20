#!/usr/bin/env python3
"""Map corpus paths to the tests that *walk those files*.

This is not line-level coverage and not a guess about `import json`.
Each rule is a test whose job is to read that corpus:

  * README.md / CONTRIBUTING.md / spec/*.md / book/ →
    documentation_examples_are_valid (it extracts ```witchy blocks from those
    trees)
  * examples/** → examples_programs plus the example_sweeps that iterate
    examples/

Std modules and compiler crates are out of scope: prelude std is in every
program, and `import json` in a snippet is not the string `std/json`. A
mention-scan would skip tests that actually import the module. LLVM
line-maps of example_tests against typeck/codegen would light up almost
every case (high fan-in), so they are not a 10x for compiler edits.

Usage:
    printf '%s\\n' <paths> | scripts/test-impact.py --example-mods
"""
from __future__ import annotations

import sys

EXAMPLE_SWEEPS = (
    "example_tests::example_sweeps::every_compilable_example_agrees_on_both_backends",
    "example_tests::example_sweeps::examples_agree_under_rc_floor",
    "example_tests::example_sweeps::every_example_agrees_under_unbox",
    "example_tests::example_sweeps::precompiled_wasm_runs_like_the_source",
    "example_tests::example_sweeps::all_examples_validate_via_cli",
    "example_tests::example_sweeps::all_example_rune_tests_pass",
)

DOC_TEST = "example_tests::example_sweeps::documentation_examples_are_valid"


def classify(paths: list[str]) -> list[str]:
    example_files = [p for p in paths if p.startswith("examples/")]
    book_files = [p for p in paths if p.startswith("book/")]
    doc_md = [
        p
        for p in paths
        if p in ("README.md", "CONTRIBUTING.md")
        or (p.startswith("spec/") and p.endswith(".md") and p != "spec/stdlib.md")
    ]

    mods: set[str] = set()
    tests: set[str] = set()

    if example_files:
        mods.add("examples_programs")
        tests.update(EXAMPLE_SWEEPS)

    if book_files or doc_md:
        tests.add(DOC_TEST)

    out: list[str] = []
    for m in sorted(mods):
        out.append(f"mod {m}")
    for t in sorted(tests):
        out.append(f"test {t}")
    return out


def main(argv: list[str]) -> int:
    if "--example-mods" not in argv:
        print("usage: test-impact.py --example-mods", file=sys.stderr)
        return 2
    paths = [ln.strip() for ln in sys.stdin if ln.strip()]
    for line in classify(paths):
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
