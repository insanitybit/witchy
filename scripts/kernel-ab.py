#!/usr/bin/env python3
"""Paired kernel A/B of two saved witchy binaries: time, allocations, retains, releases.

The metric this repo drives memory work on is allocations and refcount
operations per kernel run, not nanoseconds. For each kernel this script:

  1. runs each binary once with WITCHY_COUNT_MEMORY_OPS=1 and reads the
     exact `memory_ops allocs=.. retains=.. releases=..` line the sandbox
     prints on stderr (the counted module is slower, so it is never timed);
  2. checks both binaries print the same result lines;
  3. alternates baseline/candidate for --pairs rounds without the flag and
     records each run's self-reported `bench_ns`.

Build and save both binaries first (`cargo build --release`, copy the
executable aside). The script never rebuilds anything. Numbers vary with host
load; read the spread, and treat a single table as directional.

    scripts/kernel-ab.py --baseline /tmp/witchy-master --candidate target/release/witchy
    scripts/kernel-ab.py --baseline A --candidate B --kernels binary_trees,fannkuch --pairs 7
"""

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH_DIR = ROOT / "benchmarks"
DEFAULT_KERNELS = [
    "fib", "loop_sum", "collatz", "mandelbrot", "closure_calls", "list_sum",
    "dict_count", "binary_trees", "word_count", "expr_eval", "nsieve",
    "fannkuch", "knucleotide", "record_build",
]
MEMORY_OPS = re.compile(r"^memory_ops allocs=(\d+) retains=(\d+) releases=(\d+)$", re.M)


def run(binary, kernel, count):
    env = dict(os.environ)
    env.pop("WITCHY_COUNT_MEMORY_OPS", None)
    if count:
        env["WITCHY_COUNT_MEMORY_OPS"] = "1"
    proc = subprocess.run(
        [str(binary), "sandbox", f"{kernel}.witchy"],
        cwd=BENCH_DIR, env=env, capture_output=True, text=True, check=False,
    )
    if proc.returncode != 0:
        sys.exit(f"{binary} {kernel}: exit {proc.returncode}\n{proc.stderr}")
    lines = [line for line in proc.stdout.splitlines() if line.strip()]
    bench_ns = None
    result = []
    for line in lines:
        if line.startswith("bench_ns="):
            bench_ns = int(line.split("=", 1)[1])
        else:
            result.append(line)
    counts = None
    if count:
        match = MEMORY_OPS.search(proc.stderr)
        if not match:
            sys.exit(f"{binary} {kernel}: no memory_ops line on stderr (binary predates the counters?)\n{proc.stderr}")
        counts = tuple(int(group) for group in match.groups())
    return result, bench_ns, counts


def fmt_delta(before, after):
    if before == after:
        return f"{before}"
    sign = "-" if after < before else "+"
    pct = abs(after - before) * 100.0 / before if before else float("inf")
    return f"{before} -> {after} ({sign}{pct:.0f}%)"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--kernels", default=",".join(DEFAULT_KERNELS))
    parser.add_argument("--pairs", type=int, default=5, help="timed baseline/candidate pairs per kernel")
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--json", type=Path, help="also write raw samples here")
    args = parser.parse_args()
    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    for name, path in binaries.items():
        if not os.access(path, os.X_OK):
            parser.error(f"{name} binary is not executable: {path}")
    kernels = [kernel.strip() for kernel in args.kernels.split(",") if kernel.strip()]

    rows = []
    raw = {"baseline": str(binaries["baseline"]), "candidate": str(binaries["candidate"]),
           "pairs": args.pairs, "kernels": {}}
    header = f"{'kernel':<14} {'allocs':<28} {'retains':<28} {'releases':<28} {'ns median A':>13} {'ns median B':>13} {'B/A':>6}"
    print(header)
    print("-" * len(header))
    for kernel in kernels:
        if not (BENCH_DIR / f"{kernel}.witchy").exists():
            print(f"{kernel:<14} (missing benchmarks/{kernel}.witchy)")
            continue
        result = {}
        counts = {}
        for name, binary in binaries.items():
            result[name], _, counts[name] = run(binary, kernel, count=True)
        if result["baseline"] != result["candidate"]:
            print(f"{kernel:<14} RESULT MISMATCH baseline={result['baseline']} candidate={result['candidate']}")
            continue
        for _ in range(args.warmups):
            for binary in binaries.values():
                run(binary, kernel, count=False)
        samples = {"baseline": [], "candidate": []}
        for _ in range(args.pairs):
            for name, binary in binaries.items():
                _, bench_ns, _ = run(binary, kernel, count=False)
                if bench_ns is None:
                    sys.exit(f"{kernel}: no bench_ns line; not a kernel benchmark")
                samples[name].append(bench_ns)
        med = {name: statistics.median(values) for name, values in samples.items()}
        ratio = med["candidate"] / med["baseline"] if med["baseline"] else float("inf")
        a, b = counts["baseline"], counts["candidate"]
        print(f"{kernel:<14} {fmt_delta(a[0], b[0]):<28} {fmt_delta(a[1], b[1]):<28} {fmt_delta(a[2], b[2]):<28} "
              f"{med['baseline']:>13.0f} {med['candidate']:>13.0f} {ratio:>6.2f}")
        raw["kernels"][kernel] = {
            "counts": {"baseline": a, "candidate": b},
            "bench_ns": samples,
            "result": result["baseline"],
        }
        rows.append(kernel)
    if args.json:
        args.json.write_text(json.dumps(raw, indent=2))
        print(f"\nraw samples: {args.json}")
    if not rows:
        sys.exit("no kernel produced a comparable row")


if __name__ == "__main__":
    main()
