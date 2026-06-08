#!/usr/bin/env python3
"""Summarize hyperfine JSON exports into a markdown table written to baseline.md.

For each benchmark, reports the mean wall-clock time of each command and the
slowdown of witchy's compiled backend relative to Go (lower is better; <1.0
means witchy is faster).
"""
import json
import os
import sys


def main(benches):
    rows = []
    for b in benches:
        path = os.path.join(".build", f"{b}.json")
        try:
            with open(path) as f:
                data = json.load(f)["results"]
        except FileNotFoundError:
            continue
        # hyperfine records results in run.sh's order: native, wasm, go, [interp].
        means = [r["mean"] for r in data]
        labels = ["witchy-native", "witchy-wasm", "go", "witchy-interp"][: len(means)]
        d = dict(zip(labels, means))
        rows.append((b, d))

    lines = []
    lines.append("# witchy performance baseline")
    lines.append("")
    lines.append("Wall-clock mean (ms) per run, lower is better. **witchy-native** is the")
    lines.append("native backend (witchy -> Rust -> rustc/LLVM); **witchy-wasm** is the")
    lines.append("compiled backend (WAT -> wasmtime, with an on-disk compile cache). Both")
    lines.append("are measured as prebuilt binaries, like Go. The `vs go` columns are")
    lines.append("witchy / go (lower is better; **< 1.00 means witchy beats Go**).")
    lines.append("Regenerate with `./run.sh`.")
    lines.append("")
    lines.append("| benchmark | witchy-native (ms) | witchy-wasm (ms) | go (ms) | native vs go | wasm vs go |")
    lines.append("|-----------|-------------------:|-----------------:|--------:|-------------:|-----------:|")
    for b, d in rows:
        native = d.get("witchy-native", float("nan")) * 1000
        wasm = d.get("witchy-wasm", float("nan")) * 1000
        go = d.get("go", float("nan")) * 1000
        nratio = native / go if go else float("nan")
        wratio = wasm / go if go else float("nan")
        lines.append(
            f"| {b} | {native:.1f} | {wasm:.1f} | {go:.1f} | {nratio:.2f}x | {wratio:.2f}x |"
        )
    out = "\n".join(lines) + "\n"
    with open("baseline.md", "w") as f:
        f.write(out)
    print(out)


if __name__ == "__main__":
    main(sys.argv[1:])
