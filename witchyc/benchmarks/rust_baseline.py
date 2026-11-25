"""Persist local Benchmarks Game Rust builds and timings."""
import fcntl
import hashlib
import json
import pathlib
import platform
import shutil
import statistics
import subprocess
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
CASES = {
    "pidigits": ("pidigits-rust-4", 1000),
    "regex-redux": ("regexredux-rust-1", None),
    "reverse-complement": ("revcomp-rust-2", None),
    "trees": ("binarytrees-rust-5", 16),
    "k-nucleotide": ("knucleotide-rust-1", None),
    "fannkuch-redux": ("fannkuchredux-rust-2", 10),
    "fasta": ("fasta-rust-3", 250000),
    "mandel": ("mandelbrot-rust-1", 2000),
    "nbody": ("nbody-rust-1", 1000000),
    "spectral": ("spectralnorm-rust-1", 1000),
}
CARGO_PROGRAMS = {"pidigits-rust-4", "regexredux-rust-1", "revcomp-rust-2", "binarytrees-rust-5"}
FLAGS = ["-C", "opt-level=3", "-C", "target-cpu=native", "-C", "codegen-units=1"]


class RustBaseline:
    def __init__(self, cache, runs):
        self.cache = pathlib.Path(cache).expanduser().resolve()
        self.runs = runs
        self.compiler = subprocess.check_output(["rustc", "-vV"], text=True)
        self.mbx = HERE.parent.parent / "state/mbx/bin/mbx"

    def measure(self, name, expected, run, stdin=None):
        program, size = CASES[name]
        source = HERE / "rust" / f"{program}.rs"
        identity = {
            "source": hashlib.sha256(source.read_bytes()).hexdigest(),
            "compiler": self.compiler,
            "host": platform.platform(),
            "machine": platform.node(),
            "flags": FLAGS,
            "size": size,
        }
        if program in CARGO_PROGRAMS:
            identity["cargo"] = hashlib.sha256(
                (HERE / "rust/Cargo.toml").read_bytes() + (HERE / "rust/Cargo.lock").read_bytes()
            ).hexdigest()
        key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
        directory = self.cache / key
        directory.mkdir(parents=True, exist_ok=True)
        with (directory / "lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            return self.measure_locked(name, source, program, size, identity, directory, expected, run, stdin)

    def measure_locked(self, name, source, program, size, identity, directory, expected, run, stdin):
        artifact = directory / program
        build_record = directory / "build.json"
        output_digest = hashlib.sha256(expected).hexdigest()
        input_digest = hashlib.sha256(stdin or b"").hexdigest()
        digests = f"{input_digest}-{output_digest}" if stdin is not None else output_digest
        record = directory / f"runs-{self.runs}-{digests}.json"
        if not artifact.is_file() or not build_record.is_file():
            with tempfile.TemporaryDirectory(dir=directory) as scratch:
                binary = pathlib.Path(scratch) / program
                if program in CARGO_PROGRAMS:
                    target = self.cache / "cargo-target"
                    elapsed, built = run([
                        self.mbx, "exec", "cargo", "rustc", "--release", "--locked", "--offline",
                        "--manifest-path", HERE / "rust/Cargo.toml", "--target-dir", target,
                        "--bin", program, "--", *FLAGS,
                    ])
                    if built.returncode == 0:
                        shutil.copy2(target / "release" / program, binary)
                else:
                    elapsed, built = run([self.mbx, "exec", "rustc", *FLAGS, source, "-o", binary])
                if built.returncode:
                    raise ValueError(built.stderr.decode(errors="replace"))
                binary.replace(artifact)
                build_record.write_text(json.dumps({"compile_ms": elapsed * 1000, **identity}, indent=2) + "\n")
        build = json.loads(build_record.read_text())
        if record.is_file():
            result = json.loads(record.read_text())
        else:
            samples = []
            for index in range(self.runs + 1):
                elapsed, done = run([artifact, *([str(size)] if size is not None else [])], stdin)
                done.check_returncode()
                if done.stdout != expected:
                    raise ValueError(f"{name}: Rust output differs from Go")
                if index:
                    samples.append(elapsed * 1000)
            result = {"runtime_ms": statistics.median(samples), "samples_ms": samples,
                      "output_sha256": output_digest}
            record.write_text(json.dumps(result, indent=2) + "\n")
        if result["output_sha256"] != output_digest:
            raise ValueError(f"{name}: cached Rust output differs from Go")
        return build["compile_ms"], result["runtime_ms"]
