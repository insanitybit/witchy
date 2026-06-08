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
        # hyperfine records results in run.sh's command order: wasm, go, [interp].
        means = [r["mean"] for r in data]
        labels = ["witchy-wasm", "go", "witchy-interp"][: len(means)]
        d = dict(zip(labels, means))
        rows.append((b, d))

    lines = []
    lines.append("# witchy performance baseline")
    lines.append("")
    lines.append("Wall-clock mean (ms) per run, lower is better. witchy-wasm is the")
    lines.append("compiled backend (WAT -> wasmtime), measured end-to-end including its")
    lines.append("~5 ms per-run compile step. `vs go` is witchy-wasm / go (lower is better;")
    lines.append("< 1.00 means witchy beats Go). Regenerate with `./run.sh`.")
    lines.append("")
    has_interp = any("witchy-interp" in d for _, d in rows)
    if has_interp:
        lines.append("| benchmark | witchy-wasm (ms) | go (ms) | witchy-interp (ms) | vs go |")
        lines.append("|-----------|-----------------:|--------:|-------------------:|------:|")
    else:
        lines.append("| benchmark | witchy-wasm (ms) | go (ms) | vs go |")
        lines.append("|-----------|-----------------:|--------:|------:|")
    for b, d in rows:
        wasm = d.get("witchy-wasm", float("nan")) * 1000
        go = d.get("go", float("nan")) * 1000
        ratio = wasm / go if go else float("nan")
        if has_interp:
            interp = d.get("witchy-interp", float("nan")) * 1000
            lines.append(f"| {b} | {wasm:.1f} | {go:.1f} | {interp:.1f} | {ratio:.2f}x |")
        else:
            lines.append(f"| {b} | {wasm:.1f} | {go:.1f} | {ratio:.2f}x |")
    out = "\n".join(lines) + "\n"
    with open("baseline.md", "w") as f:
        f.write(out)
    print(out)


if __name__ == "__main__":
    main(sys.argv[1:])
