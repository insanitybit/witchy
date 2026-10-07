#!/usr/bin/env python3
"""Compare saved release compilers; record their embedded std source snapshots."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("summary", ROOT / "benchmarks/summarize.py")
summary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["baseline", "candidate", "baseline-std", "candidate-std", "output"]:
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--runs", type=int, default=12)
    parser.add_argument("--workload", action="append", choices=["packet-1000", "packet-10000", "scalar-1000", "scalar-10000", "select_fanin"])
    args = parser.parse_args()
    if args.runs < 3:
        parser.error("at least three measured pairs are required")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    variants = {
        "baseline": (args.baseline.resolve(), args.baseline_std.resolve()),
        "candidate": (args.candidate.resolve(), args.candidate_std.resolve()),
    }
    fixtures = []
    packet = (ROOT / "benchmarks/chan_scheduler.witchy").read_text()
    scalar = (ROOT / "benchmarks/chan_throughput.witchy").read_text()
    scalar = scalar.replace("async fn main(console: Console):", "async fn main(console: Console, clock: Clock):\n    let bench_t0 = clock.now_monotonic()")
    scalar += "    let bench_t1 = clock.now_monotonic()\n    console.print(\"bench_ns=${bench_t1 - bench_t0}\")\n"
    for label, source, original, checksum in [
        ("packet", packet, "10000", lambda n: n * n),
        ("scalar", scalar, "64000", lambda n: n * (n - 1) // 2),
    ]:
        for count in [1000, 10000]:
            path = output / f"{label}-{count}.witchy"
            path.write_text(source.replace(original, str(count)))
            fixtures.append((path.stem, path, str(checksum(count))))
    select = output / "select_fanin.witchy"
    select.write_bytes((ROOT / "benchmarks/select_fanin.witchy").read_bytes())
    # Source-order select computes this independent rolling checksum.
    checksum = 0
    for offset in [1, 3]:
        for index in range(2048):
            checksum = (checksum * 257 + index * 2 + offset) % 1000000007
    fixtures.append((select.stem, select, f"count=4096 checksum={checksum} order=ok"))
    if args.workload:
        fixtures = [fixture for fixture in fixtures if fixture[0] in args.workload]
    environment = os.environ.copy()
    for variable in ["WIRDIAG", "WITCHY_CANONICAL_LOWER_DEBUG", "WITCHY_PROFILE", "WITCHY_PERF_MAP", "WITCHY_DEBUG_INFO", "WITCHY_POOL"]:
        environment.pop(variable, None)
    environment["WITCHY_OPT"] = "all"
    environments = {}
    metadata = {"platform": platform.platform(), "machine": platform.machine(), "runs": args.runs,
        "warmups": 2, "optimization": "all", "cpu_count": os.cpu_count(), "load_average_start": os.getloadavg(), "native_cache": "cold per process; only the runner-owned Wasmtime cache is cleared", "std_loading": "release binaries embed std; snapshots record build provenance", "harness_sha256": digest(Path(__file__)), "variants": {}}
    for variant, (binary, std) in variants.items():
        cache = output / (variant + "-verdict-cache")
        cache_home = output / (variant + "-cache-home")
        environments[variant] = environment | {"WITCHY_SAFEWIR_CHECK_CACHE": str(cache), "XDG_CACHE_HOME": str(cache_home)}
        metadata["variants"][variant] = {"binary": str(binary), "binary_sha256": digest(binary), "std": str(std),
            "std_files": {str(path.relative_to(std)): digest(path) for path in sorted(std.rglob("*.witchy"))}}
    metadata["fixtures"] = {label: digest(path) for label, path, _ in fixtures}
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rows = []
    for label, fixture, expected in fixtures:
        samples = {variant: [] for variant in variants}
        for iteration in range(-2, args.runs):
            order = ["baseline", "candidate"] if iteration % 2 == 0 else ["candidate", "baseline"]
            for variant in order:
                prefix = output / f"{label}-{iteration}-{variant}"
                native_cache = Path(environments[variant]["XDG_CACHE_HOME"]) / "witchy/wasm"
                if native_cache.exists():
                    shutil.rmtree(native_cache)
                native_cache.mkdir(parents=True)
                start = time.monotonic_ns()
                run = subprocess.run(["/usr/bin/time", "-lp", str(variants[variant][0]), "sandbox", str(fixture)],
                    cwd=ROOT, env=environments[variant], text=True, capture_output=True, timeout=180)
                wall = time.monotonic_ns() - start
                prefix.with_suffix(".stdout").write_text(run.stdout)
                prefix.with_suffix(".stderr").write_text(run.stderr)
                kernel = re.findall(r"^bench_ns=(\d+)$", run.stdout, re.M)
                result = re.sub(r"^bench_ns=\d+\n?", "", run.stdout, flags=re.M).strip()
                if run.returncode or result != expected or len(kernel) != 1:
                    raise RuntimeError(f"{label}/{variant}: invalid result: {run.stdout!r}; {run.stderr[-1500:]}")
                rss = re.search(r"^\s*(\d+)\s+maximum resident set size$", run.stderr, re.M)
                sample = {"workload": label, "variant": variant, "iteration": iteration, "kernel_ns": int(kernel[0]),
                    "wall_ns": wall, "rss_bytes": int(rss[1]) if rss else None,
                    "native_cache_entries_after": sum(path.is_file() for path in native_cache.rglob("*"))}
                for key, name in [("instructions_retired", "instructions retired"), ("cycles_elapsed", "cycles elapsed"),
                    ("peak_memory_bytes", "peak memory footprint")]:
                    match = re.search(r"^\s*(\d+)\s+" + name + r"$", run.stderr, re.M)
                    sample[key] = int(match[1]) if match else None
                with (output / "samples.jsonl").open("a") as stream:
                    stream.write(json.dumps(sample) + "\n")
                if iteration >= 0:
                    samples[variant].append(sample)
        row = {"workload": label, "samples": samples}
        for metric in ["kernel_ns", "wall_ns", "rss_bytes", "instructions_retired", "cycles_elapsed", "peak_memory_bytes"]:
            values = {variant: [sample[metric] for sample in items] for variant, items in samples.items()}
            if all(all(value is not None for value in items) for items in values.values()):
                row[metric] = {variant: summary.sample_summary(items) for variant, items in values.items()}
                if metric in ["kernel_ns", "wall_ns", "instructions_retired", "cycles_elapsed"]:
                    ratio, interval = summary.paired_ratio_summary(values["baseline"], values["candidate"])
                    row[metric]["baseline_over_candidate"] = ratio
                    row[metric]["paired_bootstrap_95_percent_ci"] = interval
        rows.append(row)
        (output / "results.json").write_text(json.dumps({"metadata": metadata, "workloads": rows}, indent=2) + "\n")
        print(label, json.dumps(row["kernel_ns"]), flush=True)
    report = ["# Async receive frame measurements", "", "Saved release compilers with embedded std; build-source snapshots; all optimizations; warm verdict caches; cold native Wasmtime cache on both sides.", "",
        "| Workload | Before ms | After ms | Speedup (95% CI) |", "| --- | ---: | ---: | ---: |"]
    for row in rows:
        kernel = row["kernel_ns"]
        interval = kernel["paired_bootstrap_95_percent_ci"]
        report.append(f"| {row['workload']} | {kernel['baseline']['median'] / 1e6:.3f} | {kernel['candidate']['median'] / 1e6:.3f} | {kernel['baseline_over_candidate']:.2f}× ({interval[0]:.2f}–{interval[1]:.2f}) |")
    report.extend(["", "Process wall includes startup, checking, admission, JIT, execution, and the time wrapper; RSS is whole-process peak, not guest allocation bytes.",
        "Raw samples, validated outputs, source hashes, binary hashes, and std hashes are retained beside this report. Timing is not allocation-count evidence.", ""])
    metadata["load_average_end"] = os.getloadavg()
    (output / "results.json").write_text(json.dumps({"metadata": metadata, "workloads": rows}, indent=2) + "\n")
    (output / "report.md").write_text("\n".join(report))


if __name__ == "__main__":
    main()
