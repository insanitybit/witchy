#!/usr/bin/env python3
"""Compare endpoint reads while varying the number of queued messages."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parent.parent
SUMMARY_SPEC = importlib.util.spec_from_file_location("summary", ROOT / "benchmarks/summarize.py")
SUMMARY = importlib.util.module_from_spec(SUMMARY_SPEC)
SUMMARY_SPEC.loader.exec_module(SUMMARY)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=12)
    parser.add_argument("--iterations", type=int, default=1_000_000)
    args = parser.parse_args()
    if args.runs < 3 or args.iterations < 1:
        parser.error("runs must be at least 3 and iterations must be positive")
    args.output.mkdir(parents=True, exist_ok=False)
    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    metadata = {
        "platform": platform.platform(), "runs": args.runs, "warmups": 2,
        "iterations": args.iterations, "optimization": "all",
        "binaries": {name: {"path": str(path), "sha256": digest(path)}
                     for name, path in binaries.items()},
        "source_sha": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "load_start": os.getloadavg(),
    }
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    samples = []
    for buffered in [0, 64, 1024]:
        fixtures = {}
        for kind in ["owned", "borrowed"]:
            template = ROOT / "benchmarks/channel_handles" / f"read_{kind}.witchy"
            fixture = args.output / f"{kind}-{buffered}.witchy"
            fixture.write_text(template.read_text().replace("__ITERATIONS__", str(args.iterations))
                               .replace("__BUFFERED__", str(buffered)))
            fixtures[kind] = fixture
        variants = [(binary, kind) for binary in binaries for kind in fixtures]
        for iteration in range(-2, args.runs):
            order = variants if iteration % 2 == 0 else list(reversed(variants))
            for binary, kind in order:
                environment = os.environ.copy()
                for name in ["WIRDIAG", "WITCHY_CANONICAL_LOWER_DEBUG", "WITCHY_OWNER_METRICS",
                             "WITCHY_PROFILE", "WITCHY_PERF_MAP", "WITCHY_DEBUG_INFO"]:
                    environment.pop(name, None)
                environment["WITCHY_OPT"] = "all"
                environment["XDG_CACHE_HOME"] = str(args.output / f"cache-{binary}-{kind}-{buffered}-{iteration}")
                environment["WITCHY_SAFEWIR_CHECK_CACHE"] = str(args.output / f"verdict-{binary}")
                started = time.monotonic_ns()
                run = subprocess.run([str(binaries[binary]), "sandbox", str(fixtures[kind])],
                                     cwd=ROOT, env=environment, text=True, capture_output=True, timeout=180)
                wall = time.monotonic_ns() - started
                prefix = args.output / f"{binary}-{kind}-{buffered}-{iteration}"
                prefix.with_suffix(".stdout").write_text(run.stdout)
                prefix.with_suffix(".stderr").write_text(run.stderr)
                kernel = re.findall(r"^bench_ns=(\d+)$", run.stdout, re.M)
                result = re.sub(r"^bench_ns=\d+\n?", "", run.stdout, flags=re.M).strip()
                expected = f"{2 * args.iterations} {buffered * (buffered - 1) * (buffered + 1) // 3}\n0 0 1 1"
                if run.returncode or len(kernel) != 1 or result != expected or "WIRBAIL" in run.stderr:
                    raise RuntimeError(f"{prefix.name}: {run.stdout!r}; {run.stderr[-1500:]}")
                if iteration >= 0:
                    sample = {"binary": binary, "kind": kind, "buffered": buffered,
                              "iteration": iteration, "kernel_ns": int(kernel[0]), "wall_ns": wall,
                              "fixture_sha256": digest(fixtures[kind])}
                    samples.append(sample)
                    with (args.output / "samples.jsonl").open("a") as output:
                        output.write(json.dumps(sample) + "\n")
    rows = []
    for buffered in [0, 64, 1024]:
        row = {"buffered": buffered}
        for binary, kind in variants:
            values = [sample["kernel_ns"] for sample in samples
                      if (sample["binary"], sample["kind"], sample["buffered"]) == (binary, kind, buffered)]
            row[f"{binary}_{kind}"] = {"median_ns": statistics.median(values),
                                       "min_ns": min(values), "max_ns": max(values)}
        for kind in fixtures:
            before = [sample["kernel_ns"] for sample in samples
                      if sample["binary"] == "baseline" and sample["kind"] == kind and sample["buffered"] == buffered]
            after = [sample["kernel_ns"] for sample in samples
                     if sample["binary"] == "candidate" and sample["kind"] == kind and sample["buffered"] == buffered]
            ratio, interval = SUMMARY.paired_ratio_summary(before, after)
            row[f"{kind}_baseline_over_candidate"] = {"ratio": ratio, "paired_bootstrap_95_percent_ci": interval}
        rows.append(row)
    (args.output / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    print(json.dumps(rows, indent=2))


if __name__ == "__main__":
    main()
