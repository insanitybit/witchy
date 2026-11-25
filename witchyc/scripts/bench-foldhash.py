"""Measure fixed-seed byte hashing with the canonical native compiler."""
import argparse
import platform
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("witchyc", type=Path)
    args = parser.parse_args()
    target = {"Darwin": "macos-arm64", "Linux": "linux-arm64"}[platform.system()]
    source_bytes = "".join(chr(65 + (i * 7 + 11) % 26) for i in range(1025))
    with tempfile.TemporaryDirectory() as tmp:
        for size in (17, 1024):
            binaries = []
            for count in (0, 1_000_000):
                source = Path(tmp) / f"hash{size}_{count}.wy"
                binary = source.with_suffix("")
                source.write_text(f'''import foldhash
fn main(console: Console<Write>):
    let a: &str = "{source_bytes[:size]}"
    let b: &str = "{source_bytes[1:size+1]}"
    var total: U64 = 0
    for i in 0..{count}:
        if (i & 1) == 0:
            total = U64.wrapping_add(total, foldhash(a.as_bytes()))
        else:
            total = U64.wrapping_add(total, foldhash(b.as_bytes()))
    console.print_int(total)
''')
                subprocess.run([args.witchyc, "build", source, "-o", binary,
                                f"--target={target}", "--profile=release"], check=True)
                binaries.append(binary)
            samples = []
            for sample in range(5):
                durations = {}
                for i in ((0, 1) if sample % 2 == 0 else (1, 0)):
                    start = time.perf_counter_ns()
                    result = subprocess.check_output([binaries[i], "--output"])
                    durations[i] = time.perf_counter_ns() - start
                    assert (int(result) == 0) == (i == 0)
                samples.append((durations[1] - durations[0]) / 1_000_000)
            print(f"{size} bytes: {statistics.median(samples):.1f} ns/hash")

if __name__ == "__main__":
    main()
