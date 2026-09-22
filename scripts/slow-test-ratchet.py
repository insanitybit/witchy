#!/usr/bin/env python3
"""Stop NEW slow tests from landing. Target: no test over 2 seconds.

The rule "no ordinary test exceeds 2s" was documented (check.sh's
NIGHTLY-DEFERRED block, gate-test-cost.py --assert-budget) but enforced
nowhere, and by 2026-09-21 245 tests had a median over 2s -- 58% of all
test time. This is the enforcement, built so it cannot flake a serialized
gate:

  1. DETECT in the gate's own parallel run: any test over --detect seconds
     that is not exempt. Under ~8x parallel load and a sometimes-cold SafeWIR
     verdict store a 0.3s test can spike to 37s, so this alone would flake.
  2. CONFIRM each candidate by re-running it ALONE, warm and unloaded. Fail
     only if it still exceeds --confirm seconds.

Exempt: scripts/nightly-deferred-tests.txt (not in the default gate at all)
and scripts/slow-test-debt.txt (today's offenders, which may only SHRINK --
remove a line when you make that test fast; never add one).

Usage (check.sh runs this after the workspace test stage in merge-gate mode):
    scripts/slow-test-ratchet.py --junit target/nextest/default/junit.xml
"""
import argparse, os, re, subprocess, sys, time
import xml.etree.ElementTree as ET

def names(path):
    try:
        return {l.strip() for l in open(path) if l.strip() and not l.lstrip().startswith("#")}
    except FileNotFoundError:
        return set()

def isolated_seconds(binary_id, test, features):
    env = dict(os.environ, NEXTEST_STATUS_LEVEL="pass")
    cmd = ["cargo", "nextest", "run", "--workspace", "--features", features,
           "--no-fail-fast", "-E", f"binary_id(={binary_id}) and test(={test})"]
    out = subprocess.run(cmd, env=env, capture_output=True, text=True)
    m = re.findall(r"PASS \[\s*([\d.]+)s\]", out.stdout + out.stderr)
    return (float(m[-1]) if m else None), out.returncode

def main():
    ap = argparse.ArgumentParser()
    # nextest does not always write under $CARGO_TARGET_DIR: in the merge gate
    # (CARGO_TARGET_DIR=target-prewarm) it wrote <worktree>/target/nextest/...
    # Accept every plausible location and use the newest file written during
    # THIS run (--since), never a stale one from an earlier gate.
    ap.add_argument("--junit", action="append", required=True)
    ap.add_argument("--since", type=float, default=0.0,
                    help="epoch seconds; ignore JUnit files older than this")
    ap.add_argument("--detect", type=float, default=5.0)
    ap.add_argument("--confirm", type=float, default=3.0)
    ap.add_argument("--features", default="witchy-wir/native")
    ap.add_argument("--exempt", action="append",
                    default=["scripts/nightly-deferred-tests.txt", "scripts/slow-test-debt.txt"])
    a = ap.parse_args()
    fresh = [p for p in a.junit if os.path.exists(p) and os.path.getmtime(p) >= a.since]
    if not fresh:
        print("slow-test-ratchet: WARNING -- no JUnit written during this run at any of "
              + ", ".join(a.junit) + "; the ratchet checked NOTHING")
        return 0
    junit = max(fresh, key=os.path.getmtime)
    exempt = set().union(*(names(p) for p in a.exempt))
    candidates = []
    cases = list(ET.parse(junit).getroot().iter("testcase"))
    print(f"slow-test-ratchet: {len(cases)} tests in {junit}")
    for case in cases:
        name, binary = case.get("name", ""), case.get("classname", "")
        seconds = float(case.get("time") or 0)
        if seconds > a.detect and name not in exempt:
            candidates.append((seconds, binary, name))
    if not candidates:
        print(f"slow-test-ratchet: no un-exempt test over {a.detect:g}s")
        return 0
    new_slow = []
    for seconds, binary, name in sorted(candidates, reverse=True):
        alone, rc = isolated_seconds(binary, name, a.features)
        verdict = "load spike, ok" if alone is not None and alone <= a.confirm else "SLOW"
        print(f"slow-test-ratchet: {name} ({binary}): {seconds:.1f}s in gate, "
              f"{'?' if alone is None else f'{alone:.1f}s'} alone -> {verdict}")
        if alone is None or alone > a.confirm:
            new_slow.append((name, binary, alone))
    if new_slow:
        print(f"\nslow-test-ratchet: {len(new_slow)} NEW test(s) over the budget "
              f"(>{a.confirm:g}s even run alone). The target is 2s per test.\n"
              "Make it faster or split it. Deferring to scripts/nightly-deferred-tests.txt\n"
              "needs the justification check.sh's NIGHTLY-DEFERRED block asks for.\n"
              "Do NOT add it to scripts/slow-test-debt.txt: that list only shrinks.")
        return 1
    return 0

if __name__ == "__main__":
    sys.exit(main())
