#!/usr/bin/env python3
"""Mine merge-queue gate logs for per-test cost, and flag the expensive tail.

The gate's `tests (workspace)` stage is ~80% of every gate, and the suite is
CPU-bound: wall time tracks total CPU-seconds, not scheduling. Each gate log
already records a duration for every test, but nothing aggregated it, so a
2.5x regression in suite execution time accumulated unnoticed over six weeks.
This is that aggregation.

Usage:
    scripts/gate-test-cost.py                  # newest run: cost profile
    scripts/gate-test-cost.py --top 40         # deeper tail
    scripts/gate-test-cost.py --trend          # week-over-week regression
    scripts/gate-test-cost.py --budget 5.0     # list tests over a budget
"""

import argparse
import collections
import datetime
import glob
import os
import re
import statistics
import sys

def _log_dir():
    """Gate logs live in the main checkout's gitignored state/, not in a worktree."""
    override = os.environ.get("WITCHY_GATE_LOGS")
    if override:
        return override
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    candidates = [here]
    # From a worktree, .git is a file pointing at the main checkout's git dir.
    import subprocess
    try:
        common = subprocess.run(["git", "-C", here, "rev-parse", "--path-format=absolute",
                                 "--git-common-dir"], capture_output=True, text=True,
                                check=True).stdout.strip()
        if common:
            candidates.append(os.path.dirname(common))
    except Exception:
        pass
    for root in candidates:
        path = os.path.join(root, "state", "merge-queue", "logs")
        if os.path.isdir(path):
            return path
    return os.path.join(here, "state", "merge-queue", "logs")


LOG_DIR = _log_dir()

ANSI = re.compile(r"\x1b\[[0-9;]*m")
# nextest result lines: `PASS [   1.234s] (123/456) <binary> <test::name>`
RESULT = re.compile(
    r"^\s*(PASS|FAIL|SLOW|LEAK|TRY \d+ FAIL)\s*\[\s*([\d.]+)s\]"
    r"\s*\(?\s*[\d/ ]*\)?\s*(\S+)\s+(\S+)",
    re.M,
)
SUMMARY = re.compile(r"Summary \[\s*([\d.]+)s\]\s*(\d+) tests run")
LIST_DONE = re.compile(r"list progress: (\d+) test binaries started \(stage\+(\d+)s\)")


def parse(path):
    """Return a run record, or None when the log has no completed test stage."""
    try:
        text = ANSI.sub("", open(path, errors="replace").read())
    except OSError:
        return None
    summary = SUMMARY.search(text)
    if not summary:
        return None
    listed = LIST_DONE.search(text)
    tests = [(float(m.group(2)), m.group(3), m.group(4)) for m in RESULT.finditer(text)]
    return {
        "path": path,
        "mtime": os.path.getmtime(path),
        "wall": float(summary.group(1)),
        "count": int(summary.group(2)),
        "startup": int(listed.group(2)) if listed else None,
        "tests": tests,
    }


def load(limit):
    logs = sorted(glob.glob(os.path.join(LOG_DIR, "*.log")), key=os.path.getmtime)
    runs = []
    for path in reversed(logs):
        if len(runs) >= limit:
            break
        run = parse(path)
        if run and run["tests"]:
            runs.append(run)
    runs.sort(key=lambda r: r["mtime"])
    return runs


def cores():
    try:
        return os.cpu_count() or 1
    except Exception:
        return 1


def profile(run, top, budget):
    tests = sorted(run["tests"], reverse=True)
    cpu = sum(d for d, _, _ in tests)
    ncpu = cores()
    when = datetime.datetime.fromtimestamp(run["mtime"]).strftime("%Y-%m-%d %H:%M")
    print(f"gate log   {os.path.basename(run['path'])}")
    print(f"when       {when}")
    print(f"tests      {run['count']}")
    if run["startup"] is not None:
        print(f"startup    {run['startup']}s  (build + binary discovery, before any test runs)")
    print(f"execution  {run['wall']:.0f}s wall")
    print(f"cpu        {cpu:.0f}s total across all tests")
    print(f"packing    {cpu / run['wall']:.1f}x parallel on {ncpu} cores "
          f"({cpu / run['wall'] / ncpu * 100:.0f}% of theoretical)")
    print(f"floors     {cpu / ncpu:.0f}s perfect-packing floor | "
          f"{tests[0][0]:.0f}s longest-single-test floor")
    print()

    # The tail is what matters: cutting the top few tests moves the whole gate.
    print(f"--- {top} costliest tests ---")
    for duration, binary, name in tests[:top]:
        print(f"  {duration:7.1f}s  {duration / cpu * 100:4.1f}%  {binary:26} {name[:64]}")
    print()

    running = 0.0
    for i, (duration, _, _) in enumerate(tests, 1):
        running += duration
        if running > cpu * 0.5:
            print(f"50% of suite CPU is in the slowest {i} tests "
                  f"({i / len(tests) * 100:.1f}% of the suite)")
            break

    by_binary = collections.Counter()
    counts = collections.Counter()
    for duration, binary, _ in tests:
        by_binary[binary] += duration
        counts[binary] += 1
    print()
    print("--- costliest binaries ---")
    for binary, duration in by_binary.most_common(10):
        print(f"  {duration:8.1f}s  n={counts[binary]:5}  "
              f"{duration / counts[binary]:6.2f}s/test  {binary}")

    if budget:
        over = [t for t in tests if t[0] > budget]
        excess = sum(d - budget for d, _, _ in over)
        print()
        print(f"--- {len(over)} tests over the {budget:.1f}s budget "
              f"({excess:.0f}s CPU above budget, ~{excess / cores():.0f}s of wall) ---")
        for duration, binary, name in over:
            print(f"  {duration:7.1f}s  {binary:26} {name[:64]}")


def trend(runs):
    if not runs:
        print("no gate logs with test results found", file=sys.stderr)
        return
    weeks = collections.defaultdict(list)
    for run in runs:
        week = datetime.datetime.fromtimestamp(run["mtime"]).strftime("%Y-W%V")
        cpu = sum(d for d, _, _ in run["tests"])
        weeks[week].append((run["wall"], run["count"], cpu, run["startup"]))

    print(f"{'week':10} {'runs':>5} {'exec':>8} {'tests':>7} {'cpu':>8} {'s/test':>8} {'startup':>8}")
    for week in sorted(weeks):
        rows = weeks[week]
        wall = statistics.median([r[0] for r in rows])
        count = statistics.median([r[1] for r in rows])
        cpu = statistics.median([r[2] for r in rows])
        starts = [r[3] for r in rows if r[3] is not None]
        startup = f"{statistics.median(starts):.0f}s" if starts else "-"
        print(f"{week:10} {len(rows):5} {wall:7.0f}s {count:7.0f} {cpu:7.0f}s "
              f"{cpu / count:7.3f}s {startup:>8}")
    print()
    print("exec growing faster than the test count means new tests are unusually")
    print("expensive, not merely numerous — read the s/test column.")

    # Which tests grew? Compare the oldest and newest halves we have.
    if len(runs) >= 6:
        half = len(runs) // 2
        old = collections.defaultdict(list)
        new = collections.defaultdict(list)
        for run in runs[:half]:
            for duration, _, name in run["tests"]:
                old[name].append(duration)
        for run in runs[half:]:
            for duration, _, name in run["tests"]:
                new[name].append(duration)
        deltas = []
        for name, after in new.items():
            before = old.get(name)
            a = statistics.median(after)
            b = statistics.median(before) if before else 0.0
            if a - b > 1.0:
                deltas.append((a - b, b, a, name, before is None))
        deltas.sort(reverse=True)
        if deltas:
            print()
            print("--- biggest per-test growth (older half -> newer half) ---")
            for delta, b, a, name, is_new in deltas[:20]:
                tag = "NEW " if is_new else "    "
                print(f"  +{delta:6.1f}s  {tag}{b:6.1f}s -> {a:6.1f}s  {name[:64]}")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--top", type=int, default=25, help="how many costliest tests to list")
    ap.add_argument("--trend", action="store_true", help="week-over-week suite cost")
    ap.add_argument("--budget", type=float, default=0.0,
                    help="list every test slower than this many seconds")
    ap.add_argument("--runs", type=int, default=60,
                    help="how many recent gate logs to mine for --trend")
    args = ap.parse_args()

    if not os.path.isdir(LOG_DIR):
        print(f"no gate logs at {LOG_DIR} — run the merge queue first", file=sys.stderr)
        return 1

    if args.trend:
        trend(load(args.runs))
        return 0

    runs = load(1)
    if not runs:
        print("no gate log with a completed test stage found", file=sys.stderr)
        return 1
    profile(runs[-1], args.top, args.budget)
    return 0


if __name__ == "__main__":
    sys.exit(main())
