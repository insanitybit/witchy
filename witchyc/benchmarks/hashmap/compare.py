"""Compare two already-built Witchy compilers on identical mutable-map workloads."""
import argparse
import hashlib
import json
import math
import re
import shutil
from pathlib import Path
import statistics
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent
# Both sparse/mixed and dense lookups include misses. The words case exercises
# repeated borrowed Span lookups and owned String insertion. All timings include teardown.
CASES = {
    "map": (HERE.parent / "witchy/map.wy", "432577\n432577\n1000000", 1000, 64),
    "words": (HERE.parent / "witchy/words.wy", "1200000 words, 1000 distinct", 400, 4096),
    "sparse": (HERE / "sparse.wy", "537378816\n0", 200, 64),
    "dense": (HERE / "dense.wy", "90000", 200, 64),
    "clustered": (HERE / "clustered.wy", "50176", 16, 64),
    "same-hash": (HERE / "same-hash.wy", "50176", 16, 64),
    "churn": (HERE / "churn.wy", "199990000\n256", 16, 64),
}


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def paired_ratios(times):
    """Pair adjacent runs and bound the median ratio with an exact sign interval."""
    ratios = sorted(c / b for b, c in zip(times["baseline"], times["candidate"], strict=True))
    n = len(ratios)
    # A finite two-sided 95% interval requires at least six pairs.
    interval = None
    tail = 0
    for rank in range((n + 1) // 2):
        tail += math.comb(n, rank)
        if 40 * tail > 2 ** n:
            break
        interval = [ratios[rank], ratios[-1 - rank]]
    return {"median": statistics.median(ratios), "interval_95": interval}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True)
    parser.add_argument("--candidate", required=True)
    parser.add_argument("--host", help="required for wasm")
    parser.add_argument("--target", required=True, choices=["macos-arm64", "linux-arm64", "wasm"])
    parser.add_argument("--runs", type=int, default=9)
    parser.add_argument("cases", nargs="*")
    args = parser.parse_args()
    if args.runs < 1 or (args.target == "wasm" and not args.host):
        parser.error("positive run count and a host for wasm required")
    compilers = {"baseline": args.baseline, "candidate": args.candidate}
    tools = {role: sha256(shutil.which(path) or path) for role, path in compilers.items()}
    if args.target == "wasm":
        tools["host"] = sha256(shutil.which(args.host) or args.host)
    for name in args.cases or CASES:
        if name not in CASES:
            parser.error(f"unknown case {name}")
        source, expected, pages, slots = CASES[name]
        with tempfile.TemporaryDirectory() as directory:
            commands = {}
            times = {}
            phases = {}
            artifacts = {}
            for side, compiler in compilers.items():
                output = Path(directory) / side
                if args.target == "wasm":
                    output = output.with_suffix(".wasm")
                    flags = [f"--heap-pages={pages}", f"--slots={slots}"]
                    command = [args.host, output, "--time", "--unmetered", "--deadline-ms=120000", f"--memory-pages={pages * 2}"]
                else:
                    flags = [f"--target={args.target}"]
                    command = [output]
                subprocess.run([compiler, "build", source, "-o", output, "--profile=release", *flags], check=True)
                artifacts[side] = {"sha256": sha256(output), "bytes": output.stat().st_size}
                commands[side] = [*command, "--allocator", "--output"]
                times[side] = []
                phases[side] = {"startup": [], "run": []}
            for trial in range(args.runs + 1):
                for side in (reversed(commands) if trial % 2 else commands):
                    start = time.perf_counter_ns()
                    result = subprocess.run(commands[side], check=True, capture_output=True)
                    data = result.stdout
                    elapsed = (time.perf_counter_ns() - start) / 1e6
                    if data.decode().strip() != expected:
                        raise ValueError(f"{name} {side}: unexpected output {data!r}")
                    if trial:
                        times[side].append(elapsed)
                        if args.target == "wasm":
                            timing = re.search(rb"time startup ([0-9.]+) ms run ([0-9.]+) ms", result.stderr)
                            if timing is None:
                                raise ValueError(f"{side}: missing host timing {result.stderr!r}")
                            phases[side]["startup"].append(float(timing[1]))
                            phases[side]["run"].append(float(timing[2]))
            print(json.dumps(dict(case=name, target=args.target, samples_ms=times, paired_ratio=paired_ratios(times),
                                  tool_sha256=tools, source_sha256=sha256(source), artifacts=artifacts,
                                  wasm_phases_ms=phases if args.target == "wasm" else None,
                                  median_ms={k: statistics.median(v) for k, v in times.items()})), flush=True)


if __name__ == "__main__":
    main()
