#!/usr/bin/env python3
"""Measure equivalent proof-free scalar builds through linking, then run each output."""
import argparse
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import time

GO = """package main
import (
    "fmt"
    "math/bits"
)

func bump(value uint64) uint64 {
    sum, carry := bits.Add64(value, EDIT, 0)
    if carry != 0 {
        panic("integer overflow")
    }
    return sum
}

func main() { fmt.Println(bump(41)) }
"""


def timed(args, directory, env):
    start = time.perf_counter_ns()
    result = subprocess.run(args, cwd=directory, env=env, capture_output=True, check=True)
    return (time.perf_counter_ns() - start) / 1e6, result.stdout


def measure(work, compiler, go, source, n):
    env = os.environ.copy()
    for key in ("WITCHY_WASM_READY_BINDING", "WITCHY_WASM_LEAN_ROOT", "WITCHY_PROOF_CACHE"):
        env.pop(key, None)
    go_env = env | {"GOENV": "off", "GOTOOLCHAIN": "local", "GOFLAGS": "", "CGO_ENABLED": "0"}
    rows = []

    def pair(mode, sample, edit, cache):
        (work / "main.wy").write_text(source.replace("value + 1", f"value + {edit}"))
        (work / "main.go").write_text(GO.replace("EDIT", str(edit)))
        expected = f"{41 + edit}\n".encode()
        commands = [
            ("witchy", [compiler, "build", "main.wy", "-o", "witchy", "--target=macos-arm64", "--measure"], env, ["--output"]),
            ("go", [go, "build", "-o", "go", "main.go"], go_env | {"GOCACHE": str(cache)}, []),
        ]
        if sample % 2:
            commands.reverse()
        for name, command, build_env, flags in commands:
            output = work / name
            output.unlink(missing_ok=True)
            build_ms, stdout = timed(command, work, build_env)
            launch_run_ms, observed = timed([output, *flags], work, env)
            if observed != expected:
                raise ValueError(f"{name} printed {observed!r}, expected {expected!r}")
            rows.append(dict(mode=mode, sample=sample, edit=edit, tool=name, build_ms=build_ms,
                             launch_run_ms=launch_run_ms, output=observed.decode().strip(),
                             phases=json.loads(stdout) if name == "witchy" else {}))

    # Warm executable pages, the macOS toolchain, and one private Go dependency cache.
    warm = work / "go-cache"
    pair("warmup", 0, 1, warm)
    rows.clear()
    for sample in range(n):
        with tempfile.TemporaryDirectory(prefix="cold-", dir=work) as cold:
            pair("clean", sample, 1, cold)
    for sample in range(n):
        pair("warm_edit", sample, sample + 2, warm)
    return rows


def summarize(rows):
    report = {}
    for mode in ("clean", "warm_edit"):
        report[mode] = {}
        for tool in ("witchy", "go"):
            selected = [row for row in rows if row["mode"] == mode and row["tool"] == tool]
            summary = {}
            for name in ("build_ms", "launch_run_ms"):
                values = [row[name] for row in selected]
                summary[name] = dict(median=statistics.median(values), min=min(values), max=max(values))
            summary["phase_medians"] = {
                key: statistics.median(row["phases"][key] for row in selected)
                for key, value in selected[0]["phases"].items() if isinstance(value, int)
            }
            report[mode][tool] = summary
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("compiler", type=Path)
    parser.add_argument("--samples", type=int, default=9)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("--samples must be positive")
    source = (Path(__file__).resolve().parents[1] / "examples/native-scalar.wy").read_text()
    go = shutil.which("go")
    if not go:
        parser.error("go must be on PATH")
    with tempfile.TemporaryDirectory(prefix="witchy-go-") as work:
        rows = measure(Path(work), args.compiler.resolve(), go, source, args.samples)
    print(json.dumps({"go_version": subprocess.check_output([go, "version"], text=True).strip(),
                      "summary": summarize(rows), "samples": rows}, indent=2))


if __name__ == "__main__":
    main()
