#!/usr/bin/env python3
"""Write reproducible JSON and human summaries for the benchmark harness."""

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import platform
import random
import re
import shutil
import statistics
import subprocess
import sys
import tomllib


SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
ROOT_DIR = SCRIPT_DIR.parent


def command_output(*command):
    try:
        result = subprocess.run(
            command, cwd=ROOT_DIR, capture_output=True, check=False, text=True, timeout=10
        )
        output = (result.stdout or result.stderr).strip()
        return output if output else "unavailable"
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return "unavailable"


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    try:
        return sha256_bytes(path.read_bytes())
    except FileNotFoundError:
        return None


WALL_ONLY = {"chan_throughput"}
ALL_BENCHES = [
    "fib", "loop_sum", "collatz", "mandelbrot", "closure_calls", "list_sum",
    "dict_count", "binary_trees", "word_count", "expr_eval", "nsieve", "fannkuch",
    "knucleotide", "record_build", "chan_throughput", "select_fanin", "list_index",
]


def raw_kernel_samples(build_dir, benchmark, backend, expected_count):
    path = build_dir / f"{benchmark}.{backend}.kernel.tsv"
    try:
        values = []
        for line_number, line in enumerate(path.read_text().splitlines(), 1):
            fields = line.split("\t")
            if len(fields) != 2 or fields[0] != backend:
                raise ValueError(f"{path}:{line_number}: malformed {backend} sample")
            value = int(fields[1])
            if value <= 0:
                raise ValueError(f"{path}:{line_number}: sample must be positive")
            values.append(value)
    except FileNotFoundError as error:
        raise ValueError(f"missing kernel sample file: {path}") from error
    if len(values) != expected_count:
        raise ValueError(f"{path}: expected {expected_count} samples, found {len(values)}")
    return values


def wall_samples_ms(build_dir, benchmark, mode, expected_count):
    if mode != "full":
        return {"witchy": [], "go": []}
    path = build_dir / f"{benchmark}.json"
    try:
        payload = json.loads(path.read_text())
    except (FileNotFoundError, json.JSONDecodeError) as error:
        raise ValueError(f"missing or malformed current wall artifact: {path}") from error
    samples = {"witchy": [], "go": []}
    for result in payload.get("results", []):
        command = result.get("command")
        if command == "witchy-wasm":
            key = "witchy"
        elif command == "go":
            key = "go"
        else:
            raise ValueError(f"{path}: unexpected hyperfine command {command!r}")
        values = result.get("times", [])
        if len(values) != expected_count or any(value <= 0 for value in values):
            raise ValueError(f"{path}: {key} wall samples must contain {expected_count} positive values")
        samples[key] = [value * 1000 for value in values]
    if not samples["witchy"] or not samples["go"]:
        raise ValueError(f"{path}: both Witchy and Go wall samples are required")
    return samples


def percentile(sorted_values, probability):
    if not sorted_values:
        return None
    return sorted_values[round((len(sorted_values) - 1) * probability)]


def paired_ratio_summary(witchy, go):
    if len(witchy) != len(go) or not witchy:
        raise ValueError("paired Witchy and Go samples must have equal nonzero lengths")
    point = statistics.median(witchy) / statistics.median(go)
    if len(witchy) == 1:
        return point, [point, point]
    rng = random.Random(146)
    estimates = []
    for _ in range(10_000):
        indices = [rng.randrange(len(witchy)) for _ in witchy]
        w_sample = [witchy[index] for index in indices]
        g_sample = [go[index] for index in indices]
        estimates.append(statistics.median(w_sample) / statistics.median(g_sample))
    estimates.sort()
    return point, [percentile(estimates, 0.025), percentile(estimates, 0.975)]


def sample_summary(values):
    if not values:
        return None
    median = statistics.median(values)
    return {
        "median": median,
        "minimum": min(values),
        "maximum": max(values),
        "median_absolute_deviation": statistics.median(abs(value - median) for value in values),
    }


def source_identity(benchmark):
    files = []
    combined = hashlib.sha256()
    for suffix in ("witchy", "go"):
        path = SCRIPT_DIR / f"{benchmark}.{suffix}"
        if not path.exists():
            continue
        data = path.read_bytes()
        combined.update(path.name.encode())
        combined.update(b"\0")
        combined.update(data)
        files.append({"path": path.name, "sha256": sha256_bytes(data)})
    return {"files": files, "combined_sha256": combined.hexdigest()}


def git_identity():
    revision = command_output("git", "rev-parse", "HEAD")
    status = command_output("git", "status", "--porcelain=v1")
    return {"revision": revision, "dirty": status != "unavailable" and bool(status)}


def dependency_versions(package_name):
    with (ROOT_DIR / "Cargo.lock").open("rb") as lockfile:
        packages = tomllib.load(lockfile).get("package", [])
    return sorted({package["version"] for package in packages if package.get("name") == package_name})


def wasmtime_configuration():
    path = ROOT_DIR / "crates/witchy-runtime/src/runtime.rs"
    source = path.read_text()
    match = re.search(
        r"pub fn batch\(\).*?Self::with_preemption\(false, wasmtime::OptLevel::(\w+)\)",
        source,
        re.DOTALL,
    )
    if not match:
        raise ValueError("cannot derive Runtime::batch Wasmtime optimization level")
    return {
        "cranelift_opt_level": match.group(1).lower(),
        "source": str(path.relative_to(ROOT_DIR)),
        "source_sha256": sha256_file(path),
    }


def build_identity(witchy):
    binary = pathlib.Path(witchy).resolve()
    stat = binary.stat()
    return {
        "source": git_identity(),
        "witchy_binary": {
            "path": str(binary),
            "modified_unix_ns": stat.st_mtime_ns,
            "sha256": sha256_file(binary),
        },
        "toolchains": {
            "rustc": command_output("rustc", "--version", "--verbose"),
            "cargo": command_output("cargo", "--version"),
            "go": command_output("go", "version"),
            "wasmtime_cli": command_output("wasmtime", "--version"),
            "wasmtime_dependency_versions": dependency_versions("wasmtime"),
        },
        "wasmtime_configuration": wasmtime_configuration(),
        "binaryen": {
            "wasm_opt_path": shutil.which("wasm-opt"),
            "wasm_opt_version": command_output("wasm-opt", "--version"),
            "lever": os.environ.get("WITCHY_OPT", "default-on"),
        },
        "harness": {
            str(path.relative_to(ROOT_DIR)): sha256_file(path)
            for path in (ROOT_DIR / "bench.sh", SCRIPT_DIR / "run.sh", pathlib.Path(__file__).resolve())
        },
        "host": {
            "os": platform.platform(),
            "architecture": platform.machine(),
            "processor": platform.processor() or "unavailable",
        },
        "optimization_environment": {
            key: os.environ.get(key)
            for key in (
                "WITCHY_OPT",
                "WITCHY_WASM_OPT",
                "WASMTIME_OPT_LEVEL",
                "RUSTFLAGS",
                "CARGO_PROFILE_RELEASE_LTO",
                "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
            )
        },
    }


def benchmark_record(build_dir, benchmark, mode, expected_count):
    kernel_count = 0 if benchmark in WALL_ONLY else expected_count
    witchy = raw_kernel_samples(build_dir, benchmark, "witchy", kernel_count)
    go = raw_kernel_samples(build_dir, benchmark, "go", kernel_count)
    
    node_samples = []
    try:
        node_samples = raw_kernel_samples(build_dir, benchmark, "node", kernel_count)
    except ValueError:
        pass

    ruby_samples = []
    try:
        ruby_samples = raw_kernel_samples(build_dir, benchmark, "ruby", kernel_count)
    except ValueError:
        pass

    rust_samples = []
    try:
        rust_samples = raw_kernel_samples(build_dir, benchmark, "rust", kernel_count)
    except ValueError:
        pass

    def saved_result(suffix):
        try:
            return (build_dir / f"{benchmark}{suffix}.result").read_text()
        except FileNotFoundError as error:
            raise ValueError(f"missing result file for {benchmark}{suffix}") from error

    witchy_result = saved_result("")
    go_result = saved_result(".go")
    if not witchy_result or not go_result:
        raise ValueError(f"{benchmark}: Witchy and Go results must both be nonempty")
    if witchy_result != go_result:
        raise ValueError(f"{benchmark}: Witchy and Go results do not match")
    witchy_summary = sample_summary(witchy)
    go_summary = sample_summary(go)
    node_summary = sample_summary(node_samples) if node_samples else None
    ruby_summary = sample_summary(ruby_samples) if ruby_samples else None
    rust_summary = sample_summary(rust_samples) if rust_samples else None
    ratio = None
    interval = None
    if witchy and go:
        ratio, interval = paired_ratio_summary(witchy, go)
    return {
        "name": benchmark,
        "source": source_identity(benchmark),
        "results": {
            "witchy": witchy_result,
            "go": go_result,
            "match": witchy_result == go_result,
            "witchy_sha256": sha256_bytes(witchy_result.encode()),
            "go_sha256": sha256_bytes(go_result.encode()),
        },
        "kernel_ns": {
            "witchy_samples": witchy,
            "go_samples": go,
            "node_samples": node_samples,
            "ruby_samples": ruby_samples,
            "rust_samples": rust_samples,
            "witchy_summary": witchy_summary,
            "go_summary": go_summary,
            "node_summary": node_summary,
            "ruby_summary": ruby_summary,
            "rust_summary": rust_summary,
            "witchy_go_median_ratio": ratio,
            "paired_bootstrap_95_percent_ci": interval,
        },
        "wall_ms": wall_samples_ms(build_dir, benchmark, mode, expected_count),
    }


def render_markdown(artifact):
    lines = [
        "# witchy performance baseline",
        "",
        "Kernel values are medians of raw in-program compute samples. Wall values",
        "are medians of end-to-end hyperfine samples and include process startup.",
        "The machine-readable sibling artifact retains build identity, inputs,",
        "checksums, every sample, variability, and the paired ratio interval.",
        "",
        "| benchmark | kernel witchy | kernel go | kernel node | kernel ruby | kernel rust | kernel vs go | wall witchy | wall go |",
        "|-----------|--------------:|----------:|------------:|------------:|------------:|-------------:|------------:|--------:|",
    ]
    for benchmark in artifact["benchmarks"]:
        kernel = benchmark["kernel_ns"]
        ws = kernel["witchy_summary"]
        gs = kernel["go_summary"]
        ns = kernel.get("node_summary")
        rbs = kernel.get("ruby_summary")
        rss = kernel.get("rust_summary")
        wall = benchmark["wall_ms"]
        wm = statistics.median(wall["witchy"]) if wall["witchy"] else None
        gm = statistics.median(wall["go"]) if wall["go"] else None
        value = lambda number: "—" if number is None else f"{number:.1f}"
        ratio = kernel["witchy_go_median_ratio"]
        ratio_text = "—" if ratio is None else f"{ratio:.2f}x"
        lines.append(
            f"| {benchmark['name']} | {value(ws['median'] / 1e6 if ws else None)} | "
            f"{value(gs['median'] / 1e6 if gs else None)} | {value(ns['median'] / 1e6 if ns else None)} | "
            f"{value(rbs['median'] / 1e6 if rbs else None)} | {value(rss['median'] / 1e6 if rss else None)} | {ratio_text} | "
            f"{value(wm)} | {value(gm)} |"
        )
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", required=True)
    parser.add_argument("--mode", choices=("quick", "fast", "full"), required=True)
    parser.add_argument("--witchy", required=True)
    parser.add_argument("--warmup", type=int, required=True)
    parser.add_argument("--runs", type=int, required=True)
    parser.add_argument("--build-dir", required=True)
    parser.add_argument("--human")
    parser.add_argument("benchmarks", nargs="+")
    args = parser.parse_args()

    build_dir = pathlib.Path(args.build_dir).resolve()
    artifact = {
        "schema": 1,
        "created_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "mode": args.mode,
        "warmup_count": args.warmup,
        "requested_sample_count": args.runs,
        "driver_argv": [part for part in os.environ.get("BENCH_DRIVER_ARGV", "").split("\x1c") if part],
        "summarizer_argv": [str(pathlib.Path(sys.argv[0]).resolve()), *sys.argv[1:]],
        "build_identity": build_identity(args.witchy),
        "complete_suite": args.benchmarks == ALL_BENCHES,
        "sampling_order": "paired and counterbalanced; Witchy first on odd samples, Go first on even samples",
        "benchmarks": [benchmark_record(build_dir, name, args.mode, args.runs) for name in args.benchmarks],
    }
    artifact_path = pathlib.Path(args.artifact).resolve()
    artifact_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = artifact_path.with_suffix(artifact_path.suffix + ".tmp")
    temporary.write_text(json.dumps(artifact, indent=2, sort_keys=True) + "\n")
    temporary.replace(artifact_path)

    rendered = render_markdown(artifact)
    if args.human:
        pathlib.Path(args.human).write_text(rendered)
    print(rendered)


if __name__ == "__main__":
    main()
