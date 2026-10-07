#!/usr/bin/env python3
"""Render a performance-matrix JSON artifact as Markdown."""

import argparse
import json
from pathlib import Path


def median(row, variant, metric, scale=1):
    value = row["summaries"][variant].get(metric)
    return "—" if value is None else f"{value['median'] / scale:.3f}"


def ratio(row, metric, key):
    value = row["ratios"].get(metric, {}).get(key)
    return "—" if value is None else f"{value[0]:.3f} ({value[1][0]:.3f}–{value[1][1]:.3f})"


def render(artifact):
    rows = artifact["workloads"]
    metadata = artifact["metadata"]
    revisions = metadata["compiler_revisions"]
    lines = ["# Witchy performance matrix", "",
        f"Before: `{revisions['baseline']}`. Current: `{revisions['current']}`. "
        f"{metadata['go_version']}. {metadata['cpu']}, {metadata['platform']}.", "",
        f"Two warmups and {metadata['runs']} measured triplets per workload. All six orders "
        "rotate to balance variant position. Every invocation must match its independent expected result.", "",
        "Witchy uses the same checked-in sources, standard library, optimized Wasmtime "
        "configuration, and warm verdict store on both sides. Executables and source inputs "
        "are identified by SHA-256 in the JSON artifact. Admitted execution does not run Binaryen.", "",
        "## Kernel time", "",
        "Times are milliseconds inside the program's monotonic bracket. Before/current "
        "above 1 means the forwarding change is faster; current/Go below 1 favors Witchy. "
        "Parentheses contain paired bootstrap 95% intervals. An interval containing 1 "
        "does not establish a timing difference.", "",
        "| Workload | Before ms | Current ms | Go ms | Before/current | Current/Go | Changed Wasm bodies |",
        "| --- | ---: | ---: | ---: | --- | --- | ---: |"]
    for row in rows:
        cells = [row["name"], *[median(row, variant, "kernel_ns", 1e6)
            for variant in ["baseline", "current", "go"]],
            ratio(row, "kernel_ns", "baseline_over_current"),
            ratio(row, "kernel_ns", "current_over_go"), str(row["changed_code_bodies"])]
        lines.append("| " + " | ".join(cells) + " |")
    lines += ["", "Changed bodies count exact byte differences in the Wasm code section, "
        "excluding custom metadata. This is a coverage check, not a dynamic recognizer counter "
        "or an equivalence proof. `chan_throughput` has no kernel bracket.", "",
        "## Whole-process time and peak RSS", "",
        "Wall times are milliseconds from process launch through completion, without a shell. "
        "They include the resource-measurement wrapper and stdout capture. Witchy pays source "
        "compilation and JIT cost here; Go runs a precompiled binary. Peak RSS is the entire "
        "process in MiB, not guest allocation bytes. CPU samples in JSON have 0.01-second "
        "resolution from the platform time utility.", "",
        "| Workload | Before wall ms | Current wall ms | Go wall ms | Before/current wall | Before RSS MiB | Current RSS MiB | Go RSS MiB |",
        "| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: |"]
    for row in rows:
        cells = [row["name"], *[median(row, variant, "wall_ns", 1e6)
            for variant in ["baseline", "current", "go"]],
            ratio(row, "wall_ns", "baseline_over_current"),
            *[median(row, variant, "max_rss_bytes", 1024 * 1024)
                for variant in ["baseline", "current", "go"]]]
        lines.append("| " + " | ".join(cells) + " |")
    lines += ["", "## Scope and use", "",
        "The 21 standard rows characterize the existing suite. The three callback rows "
        "scale the exact mechanism this change supports. Keep these groups separate when "
        "assessing general workload impact.", "",
        "Repeat this matrix with saved compiler binaries when adding an optimization. "
        "Use the same source corpus, all variant orders, checksums, and cache policy. "
        "Do not credit small timing differences on byte-identical generated programs "
        "to this inlining rule.", "",
        "This run holds the merge-queue gate lock to avoid overlapping serialized gates; "
        "it does not reserve the whole host. Native hardware counters and guest allocation "
        "instrumentation are not collected. Prior callback native disassembly and structural "
        "checks establish the allocation-removal mechanism.", ""]
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(render(json.loads(args.artifact.read_text())))


if __name__ == "__main__":
    main()
