#!/usr/bin/env python3
"""Witchy against Go on matched programs (docs/benchmarks.md).

Builds each `go/NAME/main.go` and `witchy/NAME.wy`, checks that both print the same bytes, and
times them: Go launch to exit, Witchy launch to exit, and Witchy's startup and run apart (the
host's `--time`), so a startup change and an execution change are seen separately.

    python3 benchmarks/run.py --compiler PATH --host PATH [--runs N] [NAME ...]
    python3 benchmarks/run.py --compiler PATH --target macos-arm64 [--runs N] [NAME ...]

The compiler inherits the proof configuration it needs (for a
development build, WITCHY_WASM_READY_BINDING and WITCHY_PROOF_CACHE) already set by the caller.
"""
import argparse, hashlib, os, re, statistics, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))

# Workload inventory from the Benchmarks Game index.
WORKLOADS = {
    "fannkuch-redux": "fannkuch-redux",
    "n-body": "nbody",
    "spectral-norm": "spectral",
    "mandelbrot": "mandel",
    "fasta": "fasta",
    "k-nucleotide": "k-nucleotide",
    "reverse-complement": "reverse-complement",
    "binary-trees": "trees",
    "pidigits": "pidigits",
    "regex-redux": "regex-redux",
}

BENCHMARKS = {
    "fannkuch-redux": ([], ["--unmetered"]),
    "pidigits": (["--heap-pages=64", "--slots=256"], ["--memory-pages=80", "--allocator", "--unmetered"]),
    "regex-redux": (["--heap-pages=64", "--slots=256"], ["--memory-pages=80", "--allocator", "--unmetered"]),
    "reverse-complement": (["--heap-pages=64", "--slots=256"], ["--memory-pages=80", "--allocator", "--unmetered"]),
    "k-nucleotide": (["--heap-pages=64", "--slots=256"], ["--memory-pages=80", "--allocator", "--unmetered"]),
    "mandel": ([], ["--unmetered"]),
    "fasta": ([], ["--unmetered", "--output-bytes=4000000"]),
    "nbody": ([], ["--unmetered"]),
    "spectral": (["--heap-pages=16", "--slots=64"], ["--allocator", "--unmetered", "--memory-pages=20"]),
    "trees": (["--heap-pages=2048", "--slots=262144"], ["--allocator", "--unmetered", "--memory-pages=4096"]),
}

def run(command, stdin=None):
    started = time.perf_counter()
    done = subprocess.run(command, capture_output=True, input=stdin)
    return time.perf_counter() - started, done

def native_runs(go, witchy, count, stdin):
    times = [[], []]
    expected = None
    for pair in range(-1, count):
        commands = list(enumerate((go, witchy)))
        if pair % 2:
            commands.reverse()
        for side, command in commands:
            elapsed, done = run(command, stdin if side == 0 else None)
            if expected is None:
                expected = done.stdout
            if done.returncode != 0 or done.stdout != expected:
                return None
            if pair >= 0:
                times[side].append(elapsed * 1000)
    return tuple(map(statistics.median, times))

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--compiler", required=True)
    parser.add_argument("--host")
    parser.add_argument("--target", choices=("wasm", "macos-arm64"), default="wasm")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--rust-cache", help="compare with cached Benchmarks Game Rust baselines (native only)")
    parser.add_argument("names", nargs="*", metavar="WORKLOAD")
    options = parser.parse_args()
    native = options.target == "macos-arm64"
    if not native and options.host is None:
        parser.error("--host is required for the Wasm target")
    if options.runs < 1:
        parser.error("--runs must be positive")
    rust = None
    if options.rust_cache:
        if not native:
            parser.error("--rust-cache requires the macos-arm64 target")
        from rust_baseline import RustBaseline
        rust = RustBaseline(options.rust_cache, options.runs)
    names = options.names or list(WORKLOADS)
    for workload in names:
        if workload not in WORKLOADS:
            parser.error(f"unknown Benchmarks Game workload: {workload}")
    print("Benchmarks Game workloads at local input sizes.")
    temporary = tempfile.TemporaryDirectory(prefix="witchy-bench-")
    work = os.path.realpath(temporary.name)
    if native:
        print(f"{'program':10} {'go':>9} {'witchy':>9} {'ratio':>6}  (median of {options.runs}, launch to exit, ms)")
    else:
        print(f"{'program':10} {'go':>9} {'witchy':>9} {'ratio':>6} {'startup':>9} {'run':>9}  (best of {options.runs}, ms)")
    if rust:
        print('Rust baselines shown under matching rows (runtime and one-time build in ms).')
    failed = False
    fasta_input = None
    for workload in names:
        name = WORKLOADS[workload]
        build, flags = BENCHMARKS[name]
        go = os.path.join(work, name + "-go")
        subprocess.run(["go", "build", "-o", go, os.path.join(HERE, "go", name, "main.go")], check=True)
        stdin = None
        input_path = None
        if name in {"k-nucleotide", "reverse-complement", "regex-redux"}:
            input_path = os.path.join(work, "fasta-input")
            if fasta_input is None:
                generator = os.path.join(work, "fasta-input-generator")
                subprocess.run(["go", "build", "-o", generator, os.path.join(HERE, "go", "fasta", "main.go")], check=True)
                fasta_input = subprocess.check_output([generator, "1000"])
                with open(input_path, "wb") as input_file:
                    input_file.write(fasta_input)
            stdin = fasta_input
        source_name = "parts/spectral-paired.wy" if native and name == "spectral" else name + ".wy"
        source = (os.path.join(HERE, "..", "tests", "fixtures", "benchmarks", name + ".wy")
                  if name in {"k-nucleotide", "reverse-complement"} else os.path.join(HERE, "witchy", source_name))
        artifact = os.path.join(work, name + ("-native" if native else ".wasm"))
        if native:
            compile_cmd = [options.compiler, "build", source, "-o", artifact, "--target=macos-arm64"]
            witchy = [artifact, "--output", *(["--allocator"] if "--allocator" in flags else [])]
        else:
            compile_cmd = [options.compiler, "wasm-checked", source, "-o", artifact, *build]
            witchy = [options.host, artifact, "--output", "--deadline-ms=120000", *flags, "--time"]
        if input_path is not None:
            witchy.append("--input=" + input_path)
        built = subprocess.run(compile_cmd, capture_output=True)
        if built.returncode != 0:
            print(f"{workload:18} Witchy build failed: {built.stderr.decode(errors='replace')[-300:]}"); failed = True; continue
        go_args = {"fannkuch-redux": ["10"], "pidigits": ["1000"]}
        go_command = [go, *go_args.get(name, [])]
        if native:
            result = native_runs(go_command, witchy, options.runs, stdin)
            if result is None:
                print(f"{workload:18} execution failed or stdout differed")
                failed = True
            else:
                go_ms, native_ms = result
                print(f"{workload:18} {go_ms:9.1f} {native_ms:9.1f} {native_ms / go_ms:6.2f}")
                if rust:
                    try:
                        compile_ms, runtime_ms = rust.measure(name, subprocess.check_output(go_command, input=stdin), run, stdin)
                        print(f"  Rust {runtime_ms:.1f} ms, build {compile_ms:.1f} ms, Witchy/Rust {native_ms/runtime_ms:.2f}×")
                    except (ValueError, subprocess.CalledProcessError) as exc:
                        print(f"  Rust baseline failed: {exc}", file=sys.stderr)
                        failed = True
            continue
        go_times, witchy_times, startups, runs, outputs = [], [], [], [], set()
        for _ in range(options.runs):
            elapsed, done = run(go_command, stdin); go_times.append(elapsed); outputs.add(("go", hashlib.sha256(done.stdout).hexdigest()))
            elapsed, done = run(witchy); witchy_times.append(elapsed)
            outputs.add(("witchy", hashlib.sha256(done.stdout).hexdigest()))
            found = re.search(rb"time startup ([0-9.]+) ms run ([0-9.]+) ms", done.stderr)
            if done.returncode != 0 or not found:
                print(f"{workload:18} Witchy run failed: {done.stderr.decode(errors='replace')[-300:]}"); failed = True; break
            startups.append(float(found[1])); runs.append(float(found[2]))
        else:
            if len({digest for _, digest in outputs}) != 1:
                print(f"{workload:18} outputs differ"); failed = True; continue
            best_go, best_witchy = min(go_times) * 1e3, min(witchy_times) * 1e3
            print(f"{workload:18} {best_go:9.1f} {best_witchy:9.1f} {best_witchy / best_go:6.2f} {min(startups):9.1f} {min(runs):9.1f}")
    sys.exit(1 if failed else 0)

if __name__ == "__main__":
    main()
