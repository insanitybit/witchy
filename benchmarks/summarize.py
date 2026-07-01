#!/usr/bin/env python3
"""Summarize the benchmark run into a markdown table written to baseline.md.

Two clocks per benchmark (see run.sh):
  * kernel — in-program compute time (monotonic clock), excluding process/runtime
    startup. The headline: it isolates codegen quality. `vs go` is witchy/go
    (lower is better; < 1.00 means witchy's compiled code beats Go).
  * wall — end-to-end hyperfine mean, startup INCLUDED. The witchy wall - kernel
    gap is the fixed runtime-startup tax Go does not pay.
"""
import json
import os
import sys


def kernel_ms(b):
    """(witchy_ms, go_ms) from .build/<b>.kernel, or (None, None)."""
    try:
        with open(os.path.join(".build", f"{b}.kernel")) as f:
            w, g = f.read().split()
        to_ms = lambda s: None if s in ("", "NA") else int(s) / 1e6
        return to_ms(w), to_ms(g)
    except (FileNotFoundError, ValueError):
        return None, None


def wall_ms(b):
    """(witchy_ms, go_ms) mean wall-clock from hyperfine JSON, or (None, None)."""
    try:
        with open(os.path.join(".build", f"{b}.json")) as f:
            d = {r["command"]: r["mean"] * 1000 for r in json.load(f)["results"]}
        return d.get("witchy-wasm"), d.get("go")
    except FileNotFoundError:
        return None, None


def main(benches):
    lines = [
        "# witchy performance baseline",
        "",
        "Two clocks per benchmark. **kernel** is the compute time measured *inside*",
        "the program with a monotonic clock (witchy `now_monotonic`, Go `time.Now`),",
        "excluding process start and wasmtime instantiation — it isolates codegen",
        "quality. **wall** is the end-to-end `witchy sandbox` / Go-binary time",
        "(hyperfine mean), startup included; the witchy wall−kernel gap is the fixed",
        "runtime-startup tax. `vs go` is witchy/go on the kernel clock (lower is",
        "better; **< 1.00 means witchy beats Go**). Regenerate with `./run.sh`.",
        "",
        "| benchmark | kernel witchy | kernel go | kernel vs go | wall witchy | wall go |",
        "|-----------|--------------:|----------:|------------:|------------:|--------:|",
    ]
    ms = lambda v: "—" if v is None else f"{v:.1f}"
    for b in benches:
        kw, kg = kernel_ms(b)
        ww, wg = wall_ms(b)
        ratio = f"{kw / kg:.2f}x" if kw and kg else "—"
        lines.append(
            f"| {b} | {ms(kw)} | {ms(kg)} | {ratio} | {ms(ww)} | {ms(wg)} |"
        )
    out = "\n".join(lines) + "\n"
    with open("baseline.md", "w") as f:
        f.write(out)
    print(out)


if __name__ == "__main__":
    main(sys.argv[1:])
