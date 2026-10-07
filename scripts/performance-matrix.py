#!/usr/bin/env python3
"""Measure matched before/current/Go triplets and compare emitted Wasm bodies."""

import argparse
import hashlib
import importlib.util
import itertools
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("benchmark_summary", ROOT / "benchmarks/summarize.py")
summary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def code_bodies(data):
    position = 8

    def integer():
        nonlocal position
        value = shift = 0
        while True:
            byte = data[position]
            position += 1
            value |= (byte & 127) << shift
            if byte < 128:
                return value
            shift += 7

    if data[:8] != b"\x00asm\x01\x00\x00\x00":
        raise ValueError("invalid Wasm header")
    while position < len(data):
        section = data[position]
        position += 1
        length = integer()
        end = position + length
        if section == 10:
            count = integer()
            bodies = []
            for _ in range(count):
                size = integer()
                bodies.append(digest(data[position:position + size]))
                position += size
            if position != end:
                raise ValueError("malformed Wasm code section")
            return bodies
        position = end
    raise ValueError("missing Wasm code section")


def measure(command, environment, directory, expected, timed):
    start = time.monotonic_ns()
    process = subprocess.run(
        ["/usr/bin/time", "-lp", *command], cwd=ROOT, env=environment,
        capture_output=True, timeout=180,
    )
    wall = time.monotonic_ns() - start
    directory.with_suffix(".stderr").write_bytes(process.stderr)
    if process.returncode:
        directory.with_suffix(".stdout").write_bytes(process.stdout)
        raise RuntimeError(f"command failed: {command}; evidence: {directory}")
    if b"WIRBAIL" in process.stderr:
        raise RuntimeError(f"admission fallback: {directory}")
    clocks = re.findall(rb"(?m)^bench_ns=(\d+)\r?$", process.stdout)
    if len(clocks) != int(timed):
        raise RuntimeError(f"unexpected kernel clocks: {directory}")
    result = b"".join(line for line in process.stdout.splitlines(keepends=True)
        if not line.startswith(b"bench_ns=")).rstrip(b"\r\n")
    if result != expected:
        directory.with_suffix(".stdout").write_bytes(process.stdout)
        raise RuntimeError(f"result mismatch: {directory}")
    rss = re.search(rb"(?m)^\s*(\d+)\s+maximum resident set size", process.stderr)
    cpu = {key.decode(): float(value) for key, value in
        re.findall(rb"(?m)^(user|sys) ([0-9.]+)$", process.stderr)}
    if rss is None or set(cpu) != {"user", "sys"}:
        raise RuntimeError(f"missing resource metrics: {directory}")
    return {
        "kernel_ns": int(clocks[0]) if timed else None, "wall_ns": wall,
        "cpu_seconds": cpu["user"] + cpu["sys"], "max_rss_bytes": int(rss[1]),
        "result_sha256": digest(result),
    }


def ratio_interval(samples, numerator, denominator, metric):
    return summary.paired_ratio_summary(
        [sample[numerator][metric] for sample in samples],
        [sample[denominator][metric] for sample in samples],
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--current", type=Path, required=True)
    parser.add_argument("--baseline-revision", required=True)
    parser.add_argument("--current-revision", required=True)
    parser.add_argument("--go-artifacts", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=12)
    parser.add_argument("--only", nargs="+")
    args = parser.parse_args()
    if args.runs < 2:
        parser.error("at least two measured triplets are required")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    binaries = {"baseline": args.baseline.resolve(), "current": args.current.resolve()}
    go_artifacts = args.go_artifacts.resolve()
    environment = os.environ | {
        "WITCHY_STD_DIR": str(ROOT / "std"), "WITCHY_OPT": "all",
        "WITCHY_SAFEWIR_CHECK_CACHE": "on",
        "WITCHY_SAFEWIR_CHECK_CACHE_DIR": str(ROOT / "state/safewir-check-cache"),
        "GOMAXPROCS": str(os.cpu_count()),
    }
    environment.pop("WITCHY_PROFILE", None)
    environment.pop("WIRDIAG", None)
    cases = []
    for name in summary.ALL_BENCHES:
        cases.append({"name": name, "category": "standard suite",
            "source": ROOT / f"benchmarks/{name}.witchy",
            "go": go_artifacts / f"{name}_go",
            "go_source": ROOT / f"benchmarks/{name}.go",
            "expected": (go_artifacts / f"{name}.go.result").read_bytes(),
            "timed": name != "chan_throughput"})
    fixtures = output / "fixtures"
    fixtures.mkdir()
    for count in [200000, 2000000, 20000000]:
        name = f"scalar_callback_{count}"
        witchy = fixtures / f"{name}.witchy"
        go = fixtures / f"{name}.go"
        witchy.write_text((ROOT / "benchmarks/scalar_callback_forwarder.witchy")
            .read_text().replace("2000000", str(count)))
        go.write_text((ROOT / "benchmarks/scalar_callback_forwarder.go")
            .read_text().replace("2000000", str(count)))
        quotient, remainder = divmod(count, 7)
        cases.append({"name": name, "category": "known scalar callback",
            "source": witchy, "go": fixtures / f"{name}_go", "go_source": go,
            "expected": str(quotient * 21 + remainder * (remainder - 1) // 2).encode(),
            "timed": True})
    if args.only:
        unknown = set(args.only) - {case["name"] for case in cases}
        if unknown:
            parser.error(f"unknown workloads: {sorted(unknown)}")
        cases = [case for case in cases if case["name"] in args.only]
    for case in cases:
        if case["category"] == "known scalar callback":
            subprocess.run(["go", "build", "-o", str(case["go"]), str(case["go_source"])], check=True)
    metadata = {
        "compiler_revisions": {"baseline": args.baseline_revision, "current": args.current_revision},
        "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "platform": platform.platform(), "logical_cpus": os.cpu_count(),
        "cpu": subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip(),
        "rustc_version": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "go_version": subprocess.check_output(["go", "version"], text=True).strip(),
        "runs": args.runs, "warmups": 2,
        "binary_sha256": {key: digest(path.read_bytes()) for key, path in binaries.items()},
        "harness_sha256": digest(Path(__file__).read_bytes()),
        "environment": {key: environment.get(key) for key in ["WITCHY_STD_DIR", "WITCHY_OPT",
            "WITCHY_SAFEWIR_CHECK_CACHE", "WITCHY_SAFEWIR_CHECK_CACHE_DIR", "GOMAXPROCS", "GOGC", "GODEBUG"]},
        "wall_method": "monotonic process launch, no shell; includes time wrapper and stdout capture",
        "rss_method": "Darwin /usr/bin/time -l, entire compiler/JIT/guest process",
    }
    orders = list(itertools.permutations(["baseline", "current", "go"]))
    metadata["order"] = orders
    stdlib = hashlib.sha256()
    for path in sorted((ROOT / "std").rglob("*.witchy")):
        stdlib.update(str(path.relative_to(ROOT)).encode())
        stdlib.update(b"\0")
        stdlib.update(path.read_bytes())
    metadata["stdlib_sha256"] = stdlib.hexdigest()
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rows = []
    for case in cases:
        directory = output / case["name"]
        directory.mkdir()
        case_bodies = {}
        for variant, binary in binaries.items():
            wasm = directory / f"{variant}.wasm"
            compiled = subprocess.run([str(binary), "compile", str(case["source"]), "--out", str(wasm)],
                cwd=ROOT, env=environment, capture_output=True, timeout=180)
            (directory / f"{variant}.compile.stderr").write_bytes(compiled.stderr)
            if compiled.returncode:
                raise RuntimeError(f"failed Wasm inspection: {case['name']}/{variant}")
            case_bodies[variant] = code_bodies(wasm.read_bytes())
        before, current = case_bodies["baseline"], case_bodies["current"]
        changed = sum(left != right for left, right in zip(before, current)) if len(before) == len(current) else None
        commands = {variant: [str(binary), "sandbox", str(case["source"])]
            for variant, binary in binaries.items()}
        commands["go"] = [str(case["go"])]
        samples = []
        for run in range(-2, args.runs):
            order = orders[(run + 2) % len(orders)]
            sample = {"run": run, "order": order}
            for variant in order:
                sample[variant] = measure(commands[variant], environment,
                    directory / f"{run + 2:02}-{variant}", case["expected"], case["timed"])
            if run >= 0:
                samples.append(sample)
                with (output / "samples.jsonl").open("a") as stream:
                    stream.write(json.dumps({"workload": case["name"], **sample}) + "\n")
        metric_summaries = {variant: {metric: summary.sample_summary([sample[variant][metric] for sample in samples])
            for metric in ["wall_ns", "cpu_seconds", "max_rss_bytes"] + (["kernel_ns"] if case["timed"] else [])}
            for variant in commands}
        row = {
            "name": case["name"], "category": case["category"],
            "source_sha256": digest(case["source"].read_bytes()),
            "go_source_sha256": digest(case["go_source"].read_bytes()),
            "go_binary_sha256": digest(case["go"].read_bytes()),
            "expected_result_sha256": digest(case["expected"]),
            "code_body_hashes": case_bodies, "changed_code_bodies": changed,
            "samples": samples, "summaries": metric_summaries,
            "ratios": {metric: {
                "baseline_over_current": ratio_interval(samples, "baseline", "current", metric),
                "current_over_go": ratio_interval(samples, "current", "go", metric),
            } for metric in ["wall_ns"] + (["kernel_ns"] if case["timed"] else [])},
        }
        rows.append(row)
        (output / "matrix.json").write_text(json.dumps({"metadata": metadata, "workloads": rows}, indent=2) + "\n")
        selected_metric = "kernel_ns" if case["timed"] else "wall_ns"
        ratio, interval = row["ratios"][selected_metric]["baseline_over_current"]
        print(f"{case['name']}: before/current={ratio:.3f} [{interval[0]:.3f}, {interval[1]:.3f}], changed bodies={changed}", flush=True)


if __name__ == "__main__":
    main()
