#!/usr/bin/env python3
"""What a build costs by phase, and in instructions, for a list of programs.

usage: library-cost.py COMPILER RUNS PROGRAM.wy...

Per program: the median of each `wasm-measure` phase (ms), then the medians of instructions
retired and wall time of `wasm-checked` (`/usr/bin/time -l`). Compare a program with one that
only imports the same modules (`import console` and an empty `main`) to see what the library
costs every build, and with a two-line program for the cost of the process itself
(docs/benchmarks.md, 2026-11-21). The environment selects the checker binding and proof cache.
"""
import json, os, re, statistics, subprocess, sys, tempfile, time

compiler, runs, programs = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
with tempfile.TemporaryDirectory() as scratch:
    out = os.path.join(scratch, "m.wasm")
    for program in programs:
        phases, instructions, wall, counts = {}, [], [], None
        for _ in range(runs):
            done = subprocess.run([compiler, "wasm-measure", program, "-o", out], capture_output=True, text=True)
            if not done.stdout.startswith("{"):
                print(f"{program}: not built: {done.stderr[:200]}"); break
            m = json.loads(done.stdout); check, proof = m["phases"]["static_check"], m["phases"]["fresh_proof_check"]
            row = {"read": m["read"], "parse": m["parse"], "typed": check["primary_typed_check"], "goals": check["vc_generation"],
                   "concrete": check["concrete_planning"], "proof": m["fresh_proof_check"], "emit": m["emit"], "receipt": m["receipt"], "publish": m["publish"]}
            for name, value in row.items(): phases.setdefault(name, []).append(value / 1e6)
            counts = (m["source_bytes"], proof["counts"]["selected_goals"], m["wasm_bytes"])
            start = time.perf_counter()
            timed = subprocess.run(["/usr/bin/time", "-l", compiler, "wasm-checked", program, "-o", out], capture_output=True, text=True)
            wall.append((time.perf_counter() - start) * 1000)
            found = re.search(r"(\d+)\s+instructions retired", timed.stderr)
            if found: instructions.append(int(found.group(1)) / 1e6)
        else:
            median = {name: statistics.median(values) for name, values in phases.items()}
            print(f"{program}\n  " + "  ".join(f"{name} {value:.2f}" for name, value in median.items()) + f"  sum {sum(median.values()):.2f} ms")
            print(f"  {statistics.median(instructions) if instructions else 0:.1f} M instructions, wall {statistics.median(wall):.1f} ms; {counts[0]} source bytes, {counts[1]} goals, {counts[2]} module bytes")
