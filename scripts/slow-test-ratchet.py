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
  2. CONFIRM the candidates by re-running them ALONE: one nextest run over all
     of them with --test-threads 1, using the gate's own selection and
     features (--nextest-args) so it builds and lists nothing new. Fail only
     for a test that still exceeds --confirm seconds.

The confirmation streams every result as it lands (a silent stage is killed
by the queue's stall detector: that took down a green gate on 2026-09-23, when
each candidate got its own --workspace run) and stops at --budget seconds. A
test the budget did not reach is reported and NOT failed: a measurement must
never be what kills a gate whose tests passed.

Exempt: scripts/nightly-deferred-tests.txt (not in the default gate at all)
and scripts/slow-test-debt.txt (today's offenders, which may only SHRINK --
remove a line when you make that test fast; never add one).

Usage (check.sh runs this after the workspace test stage in merge-gate mode):
    scripts/slow-test-ratchet.py --junit target/nextest/default/junit.xml
"""
import argparse, os, re, shlex, subprocess, sys, time
import xml.etree.ElementTree as ET

def names(path):
    try:
        return {l.strip() for l in open(path) if l.strip() and not l.lstrip().startswith("#")}
    except FileNotFoundError:
        return set()

PASS_LINE = re.compile(r"\b(PASS|FAIL|SLOW)\s*\[\s*([\d.]+)s\]\s+(?:\(\s*\d+/\d+\)\s+)?(\S+)\s+(\S+)")

def confirm_alone(candidates, nextest_args, budget):
    """Run every candidate once, one at a time, in a single nextest run.
    Returns ({(binary, test): seconds or None}, finished_within_budget)."""
    expr = " or ".join(f"(binary_id(={binary}) and test(={name}))" for _, binary, name in candidates)
    cmd = ["cargo", "nextest", "run", *nextest_args, "--no-fail-fast",
           "--test-threads", "1", "-E", expr]
    env = dict(os.environ, NEXTEST_STATUS_LEVEL="pass")
    print("slow-test-ratchet: confirming alone: " + " ".join(shlex.quote(c) for c in cmd), flush=True)
    seconds = {}
    started = time.monotonic()
    process = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               text=True, bufsize=1)
    # The deadline must hold even if one test runs silently past it.
    expired = []
    def stop():
        expired.append(True)
        process.kill()
    import threading
    timer = threading.Timer(budget, stop)
    timer.start()
    within = True
    for line in process.stdout:
        match = PASS_LINE.search(line)
        if match:
            status, secs, binary, name = match.groups()
            seconds[(binary, name)] = float(secs) if status != "FAIL" else None
            print(f"slow-test-ratchet:   {status} {float(secs):.1f}s alone {binary} {name}", flush=True)
        if time.monotonic() - started > budget:
            within = False
            process.kill()
            break
    process.wait()
    timer.cancel()
    return seconds, within and not expired

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
    ap.add_argument("--nextest-args", default="",
                    help="the gate's nextest selection and features; default --workspace")
    ap.add_argument("--budget", type=float, default=300.0,
                    help="seconds for the whole confirmation run")
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
    nextest_args = shlex.split(a.nextest_args) or ["--workspace", "--features", "witchy-wir/native"]
    alone, within = confirm_alone(sorted(candidates, reverse=True), nextest_args, a.budget)
    new_slow = []
    for seconds, binary, name in sorted(candidates, reverse=True):
        if (binary, name) not in alone:
            print(f"slow-test-ratchet: {name} ({binary}): {seconds:.1f}s in gate, "
                  "not reached within the confirmation budget -> NOT failed")
            continue
        secs = alone[(binary, name)]
        verdict = "load spike, ok" if secs is not None and secs <= a.confirm else "SLOW"
        print(f"slow-test-ratchet: {name} ({binary}): {seconds:.1f}s in gate, "
              f"{'failed' if secs is None else f'{secs:.1f}s'} alone -> {verdict}")
        if secs is None or secs > a.confirm:
            new_slow.append((name, binary, secs))
    if not within:
        print(f"slow-test-ratchet: WARNING -- confirmation stopped at the {a.budget:g}s budget; "
              "unreached candidates were not judged")
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
