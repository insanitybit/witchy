#!/usr/bin/env python3
"""Check the stage-crate dependency contract without building Rust tests."""

import json
import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def fail(message: str) -> None:
    print(f"stage-dependencies: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    expected = json.loads((ROOT / "spec/stage-dependencies.json").read_text())
    cargo = os.environ.get("CARGO", "cargo")
    result = subprocess.run(
        [cargo, "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode:
        fail(f"cargo metadata failed:\n{result.stderr.rstrip()}")

    packages = json.loads(result.stdout)["packages"]
    actual = {}
    for package in packages:
        name = package["name"]
        if not name.startswith("witchy-"):
            continue
        actual[name] = sorted(
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency.get("path") is not None
            and dependency.get("kind") is None
            and dependency["name"].startswith("witchy-")
        )

    missing = sorted(set(expected) - set(actual))
    unexpected = sorted(set(actual) - set(expected))
    if missing or unexpected:
        fail(
            "workspace stage crates changed; update spec/architecture-ledger.md "
            f"and spec/stage-dependencies.json together (missing={missing}, unexpected={unexpected})"
        )

    # RFC-0170 P4: witchy-wir must not reach the checker even through a
    # dev-dependency. Cargo applies one to every test target, so a typeck edit
    # would rebuild wir's test binaries.
    for package in packages:
        if package["name"] == "witchy-wir" and any(
            dependency["name"] == "witchy-types" for dependency in package["dependencies"]
        ):
            fail("witchy-wir depends on witchy-types (in some dependency table); see RFC-0170 P4")

    violations = []
    for stage, dependencies in actual.items():
        forbidden = sorted(set(dependencies) - set(expected[stage]))
        if forbidden:
            violations.append(f"{stage} gained forbidden stage dependencies {forbidden}")
    if violations:
        fail("; ".join(violations))


if __name__ == "__main__":
    main()
