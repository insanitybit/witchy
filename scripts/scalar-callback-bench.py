#!/usr/bin/env python3
"""Alternate saved compiler binaries on the scalar callback fixture."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import random
import statistics
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--baseline", type=Path, required=True)
parser.add_argument("--candidate", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--rounds", type=int, default=21)
parser.add_argument("--iterations", type=int, default=2000000)
parser.add_argument("--optimizations", choices=["all", "all,-wasm-opt"], default="all")
args = parser.parse_args()
if args.rounds < 3:
    parser.error("at least three rounds are required")
if args.iterations < 1:
    parser.error("iterations must be positive")
root = Path(__file__).resolve().parent.parent
args.output.mkdir(parents=True, exist_ok=False)
fixture = args.output.resolve() / "fixture.witchy"
fixture.write_text((root / "benchmarks/scalar_callback_forwarder.witchy").read_text().replace("2000000", str(args.iterations)))
quotient, remainder = divmod(args.iterations, 7)
expected = quotient * 21 + remainder * (remainder - 1) // 2
binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
env = os.environ | {
    "WITCHY_STD_DIR": str(root / "std"),
    "WITCHY_OPT": args.optimizations,
    "WITCHY_SAFEWIR_CHECK_CACHE": "off",
}
env.pop("WIRDIAG", None)
env.pop("WITCHY_PROFILE", None)
metadata = {
    "platform": platform.platform(),
    "machine": platform.machine(),
    "environment": {key: env[key] for key in ["WITCHY_STD_DIR", "WITCHY_OPT", "WITCHY_SAFEWIR_CHECK_CACHE"]},
    "binary_sha256": {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()},
    "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
    "fixture_sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(),
    "iterations": args.iterations,
}
(args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
samples = []
for round_number in range(args.rounds + 1):
    order = ["baseline", "candidate"] if round_number % 2 == 0 else ["candidate", "baseline"]
    for variant in order:
        start = time.monotonic_ns()
        run = subprocess.run(
            [str(binaries[variant]), "sandbox", str(fixture)],
            cwd=root, env=env, text=True, capture_output=True,
        )
        wall_ns = time.monotonic_ns() - start
        prefix = args.output / f"{round_number:02}-{variant}"
        prefix.with_suffix(".stdout").write_text(run.stdout)
        prefix.with_suffix(".stderr").write_text(run.stderr)
        if run.returncode:
            raise RuntimeError(f"{variant} failed: {run.stderr}")
        match = re.fullmatch(str(expected) + r"\nbench_ns=(\d+)\n", run.stdout)
        if match is None or "WIRBAIL" in run.stderr:
            raise RuntimeError(f"invalid measurement: {run.stdout} {run.stderr}")
        if round_number:
            samples.append({"round": round_number, "variant": variant, "runtime_ns": int(match[1]), "wall_ns": wall_ns})
            (args.output / "samples.json").write_text(json.dumps(samples, indent=2) + "\n")
for variant in binaries:
    values = [sample["runtime_ns"] for sample in samples if sample["variant"] == variant]
    print(f"{variant}: median={statistics.median(values):.0f} ns min={min(values)} max={max(values)}")
baseline = [sample["runtime_ns"] for sample in samples if sample["variant"] == "baseline"]
candidate = [sample["runtime_ns"] for sample in samples if sample["variant"] == "candidate"]
rng = random.Random(0)
ratios = []
for _ in range(10000):
    indices = [rng.randrange(args.rounds) for _ in range(args.rounds)]
    ratios.append(statistics.median([baseline[index] for index in indices]) / statistics.median([candidate[index] for index in indices]))
ratios.sort()
summary = {
    "speedup": statistics.median(baseline) / statistics.median(candidate),
    "paired_bootstrap_95_percent_interval": [ratios[250], ratios[9750]],
    "wall_median_ns": {variant: statistics.median(sample["wall_ns"] for sample in samples if sample["variant"] == variant) for variant in binaries},
}
(args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
print(json.dumps(summary, indent=2))
