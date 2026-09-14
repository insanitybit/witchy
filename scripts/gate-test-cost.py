#!/usr/bin/env python3
"""Mine merge-queue gate logs for per-test cost, and flag the expensive tail.

Each gate log records wall duration for every test. Those durations overlap
when nextest runs tests concurrently, and individual tests can spend time
waiting or running subprocesses, so their sum is neither gate wall time nor
CPU time. This report uses the sum only to rank expensive tests and binaries;
the nextest summary remains the authoritative execution wall time.

Usage:
    scripts/gate-test-cost.py                  # newest run: cost profile
    scripts/gate-test-cost.py --top 40         # deeper tail
    scripts/gate-test-cost.py --trend          # week-over-week regression
    scripts/gate-test-cost.py --workspace-only # ignore scoped landing shards
    scripts/gate-test-cost.py --log state/merge-queue/logs/gate.log # inspect one named log
    scripts/gate-test-cost.py --budget 5.0     # list tests over a budget
    scripts/gate-test-cost.py --assert-budget 2 # fail if any ordinary test exceeds 2s
    scripts/gate-test-cost.py --assert-budget 2 --exempt-file scripts/nightly-deferred-tests.txt
        # same, but skip the tests scripts/check.sh already excludes from the
        # default gate (see NIGHTLY-DEFERRED there) -- run this against a
        # --full or CI gate log, where those tests DO appear, to catch a NEW
        # test crossing the budget without re-litigating the deferred list.

--assert-budget mines the SINGLE newest gate log, whatever kind it is. A
default/--fast merge-gate is often SCOPED to the touched paths (see
WITCHY_GATE_SCOPE / the coordinator's batch classification in
merge-queue.sh) and runs a different test subset each time -- its "over
budget" set is run-dependent, not a stable enforcement signal. Point this
at a --full or CI log (the whole workspace, every time) before treating a
failure as real, and before wiring this into any automated gate.
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
# Terminal nextest result lines: `PASS [ 1.234s] (123/456) <binary> <test::name>`.
# `SLOW` is a progress notification for a still-running test; counting it and
# the later PASS would double-count one attempt.
RESULT = re.compile(
    r"^\s*(PASS|FAIL|LEAK|TRY \d+ FAIL)\s*\[\s*([\d.]+)s\]"
    r"\s*\(?\s*[\d/ ]*\)?\s*(\S+)\s+(\S+)",
)
SUMMARY = re.compile(r"Summary \[\s*([\d.]+)s\]\s*(\d+) tests? run")
ALL_BINARIES_STARTED = re.compile(
    r"list progress: (\d+) test binaries started \(stage\+(\d+)s\)"
)
STAGE = re.compile(r"^==> \[\d+\] (.+?)(?: \(t\+\d+s\))?$")


def parse(path):
    """Return a run record, or None when the log has no completed test stage."""
    try:
        text = ANSI.sub("", open(path, errors="replace").read())
    except OSError:
        return None
    segments = []
    tests = []
    current_stage = None
    for line in text.splitlines():
        stage = STAGE.match(line)
        if stage:
            current_stage = stage.group(1)
        result = RESULT.match(line)
        if result:
            tests.append((float(result.group(2)), result.group(3), result.group(4)))
        summary = SUMMARY.search(line)
        if summary:
            if tests:
                segments.append((float(summary.group(1)), int(summary.group(2)), tests, current_stage))
            tests = []
    if not segments:
        return None
    selected_index, selected = max(enumerate(segments), key=lambda pair: pair[1][0])
    wall, count, tests, selected_stage = selected
    listed = ALL_BINARIES_STARTED.search(text) if selected_index == 0 else None
    return {
        "path": path,
        "mtime": os.path.getmtime(path),
        "wall": wall,
        "count": count,
        "binaries_started": int(listed.group(1)) if listed else None,
        "all_binaries_started_stage_s": int(listed.group(2)) if listed else None,
        "tests": tests,
        "segment": selected_index + 1,
        "segments": len(segments),
        "scoped": "check.sh: serialized nextest selection:" in text,
        "stage": selected_stage,
        "workspace": (
            selected_stage == "tests (workspace)"
            and "check.sh: serialized nextest selection:" not in text
        ) or (selected_stage is None and "check.sh: serialized nextest selection:" not in text),
    }


def load(limit, workspace_only=False):
    logs = sorted(glob.glob(os.path.join(LOG_DIR, "*.log")), key=os.path.getmtime)
    runs = []
    for path in reversed(logs):
        if len(runs) >= limit:
            break
        run = parse(path)
        if run and run["tests"] and (not workspace_only or run["workspace"]):
            runs.append(run)
    runs.sort(key=lambda r: r["mtime"])
    return runs


def load_exempt(path):
    """One test name per line; blank lines and '#' comments ignored."""
    if not path:
        return set()
    names = set()
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            names.add(line)
    return names


def profile(run, top, budget, exempt=None):
    tests = sorted(run["tests"], reverse=True)
    test_seconds = sum(d for d, _, _ in tests)
    when = datetime.datetime.fromtimestamp(run["mtime"]).strftime("%Y-%m-%d %H:%M")
    print(f"gate log   {os.path.basename(run['path'])}")
    print(f"when       {when}")
    if run["stage"]:
        print(f"stage      {run['stage']}")
    if run["segments"] > 1:
        print(f"nextest    {run['segment']}/{run['segments']} slowest completed invocation")
    print(f"tests      {run['count']}")
    if run["all_binaries_started_stage_s"] is not None:
        print(
            f"discovery  all {run['binaries_started']} binaries observed started by "
            f"stage+{run['all_binaries_started_stage_s']}s (upper bound; overlaps tests)"
        )
    print(f"execution  {run['wall']:.0f}s wall")
    print(f"test time  {test_seconds:.0f}s summed per-test wall (overlaps; not CPU)")
    print(f"overlap    {test_seconds / run['wall']:.1f}x summed-test-time / execution wall")
    print(f"longest    {tests[0][0]:.0f}s single test")
    print()

    # Nextest's `(N/total)` progress count is completion order. These are the
    # tests that actually finish nearest the stage boundary; their durations
    # must not be summed as a gate duration because other tests overlap them.
    print(f"--- final completion tail ({min(top, len(tests))} tests) ---")
    completion = run["tests"]
    for offset, (duration, binary, name) in enumerate(reversed(completion[-top:]), 1):
        print(f"  {offset:2d} from end  {duration:7.1f}s  {binary:26} {name[:64]}")
    print()

    # The tail is what matters: cutting the top few tests moves the whole gate.
    print(f"--- {top} costliest tests ---")
    for duration, binary, name in tests[:top]:
        print(f"  {duration:7.1f}s  {duration / test_seconds * 100:4.1f}%  {binary:26} {name[:64]}")
    print()

    running = 0.0
    for i, (duration, _, _) in enumerate(tests, 1):
        running += duration
        if running > test_seconds * 0.5:
            print(f"50% of summed test time is in the slowest {i} tests "
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

    over = []
    if budget:
        exempt = exempt or set()
        all_over = [t for t in tests if t[0] > budget]
        over = [t for t in all_over if t[2] not in exempt]
        skipped = len(all_over) - len(over)
        excess = sum(d - budget for d, _, _ in over)
        print()
        exempt_note = f", {skipped} exempted" if skipped else ""
        print(f"--- {len(over)} tests over the {budget:.1f}s budget{exempt_note} "
              f"({excess:.0f}s summed test time above budget) ---")
        for duration, binary, name in over:
            print(f"  {duration:7.1f}s  {binary:26} {name[:64]}")
    return over


def trend(runs):
    if not runs:
        print("no gate logs with test results found", file=sys.stderr)
        return
    weeks = collections.defaultdict(list)
    for run in runs:
        week = datetime.datetime.fromtimestamp(run["mtime"]).strftime("%Y-W%V")
        test_seconds = sum(d for d, _, _ in run["tests"])
        weeks[week].append((
            run["wall"],
            run["count"],
            test_seconds,
            run["all_binaries_started_stage_s"],
        ))

    print(f"{'week':10} {'runs':>5} {'exec':>8} {'tests':>7} {'test-s':>8} {'s/test':>8} {'all-bin':>8}")
    for week in sorted(weeks):
        rows = weeks[week]
        wall = statistics.median([r[0] for r in rows])
        count = statistics.median([r[1] for r in rows])
        test_seconds = statistics.median([r[2] for r in rows])
        starts = [r[3] for r in rows if r[3] is not None]
        all_binaries = f"{statistics.median(starts):.0f}s" if starts else "-"
        print(f"{week:10} {len(rows):5} {wall:7.0f}s {count:7.0f} {test_seconds:7.0f}s "
              f"{test_seconds / count:7.3f}s {all_binaries:>8}")
    print()
    print("Summed per-test time growing faster than the test count means the")
    print("observed tests are unusually expensive; read it beside execution wall.")
    print("all-bin is a heartbeat-sampled upper bound for all binaries starting;")
    print("tests can already be running, so it is not a startup-only duration.")

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
    ap.add_argument("--workspace-only", action="store_true",
                    help="ignore path-scoped nextest selections and mine full workspace stages")
    ap.add_argument("--log", type=str, default="",
                    help="inspect exactly one existing gate log instead of selecting the newest")
    ap.add_argument("--budget", type=float, default=0.0,
                    help="list every test slower than this many seconds")
    ap.add_argument("--assert-budget", type=float, default=0.0,
                    help="fail if any test in the newest completed gate exceeds this many seconds")
    ap.add_argument("--exempt-file", type=str, default="",
                    help="path to a file of test names (one per line) to skip "
                         "when applying --assert-budget/--budget")
    ap.add_argument("--runs", type=int, default=60,
                    help="how many recent gate logs to mine for --trend")
    args = ap.parse_args()

    if not os.path.isdir(LOG_DIR):
        print(f"no gate logs at {LOG_DIR} — run the merge queue first", file=sys.stderr)
        return 1

    if args.trend and args.log:
        ap.error("--log cannot be combined with --trend")

    if args.trend:
        trend(load(args.runs, args.workspace_only))
        return 0

    if args.log:
        run = parse(os.path.abspath(args.log))
        runs = [run] if run else []
    else:
        runs = load(1, args.workspace_only)
    if not runs:
        print("no gate log with a completed test stage found", file=sys.stderr)
        return 1
    budget = args.assert_budget or args.budget
    exempt = load_exempt(args.exempt_file)
    over = profile(runs[-1], args.top, budget, exempt)
    if args.assert_budget and over:
        print(
            f"FAIL: {len(over)} tests exceed the {args.assert_budget:.1f}s hard ceiling",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
