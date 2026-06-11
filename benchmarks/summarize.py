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
        # Map by the hyperfine command label (`-n NAME`).
        d = {r["command"]: r["mean"] for r in data}
        rows.append((b, d))

    lines = []
    lines.append("# witchy performance baseline")
    lines.append("")
    lines.append("Wall-clock mean (ms) per run, lower is better. **witchy-wasm** is the")
    lines.append("compiled tier (WAT -> wasmtime, with an on-disk compile cache),")
    lines.append("measured end-to-end via `witchy sandbox`, including process start and")
    lines.append("instantiation — like the prebuilt Go binary it races. `vs go` is")
    lines.append("witchy-wasm / go (lower is better; **< 1.00 means witchy beats Go**).")
    lines.append("Regenerate with `./run.sh`.")
    lines.append("")
    lines.append("| benchmark | witchy-wasm (ms) | go (ms) | vs go |")
    lines.append("|-----------|-----------------:|--------:|------:|")

    def fmt(d, key):
        return f"{d[key] * 1000:.1f}" if key in d else "—"

    for b, d in rows:
        go = d.get("go")
        ratio = f"{d['witchy-wasm'] / go:.2f}x" if go and "witchy-wasm" in d else "—"
        lines.append(f"| {b} | {fmt(d, 'witchy-wasm')} | {fmt(d, 'go')} | {ratio} |")
    out = "\n".join(lines) + "\n"
    with open("baseline.md", "w") as f:
        f.write(out)
    print(out)


if __name__ == "__main__":
    main(sys.argv[1:])
