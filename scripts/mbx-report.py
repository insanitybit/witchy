#!/usr/bin/env python3
"""Summarize the mbx build reports that scripts/bin/cargo writes.

Each build through the cargo shim leaves one versioned JSON report (mbx's
MBX_STATS_REPORT) under state/mbx/reports/, named
<utc-time>-<pid>-<mode>-<subcommand>-<worktree>.json. This script turns them
into the numbers that answer "is the cache making builds faster":

  ./scripts/mbx-report.py                 # last 24 hours, totals + per worktree
  ./scripts/mbx-report.py --since 7d --by day
  ./scripts/mbx-report.py --since 24h --by mode
  ./scripts/mbx-report.py --json          # machine-readable totals

Compiler time avoided is mbx's own estimate (the recorded duration of the
compilation a hit replaced); compiler time spent is measured. Both are
summed across parallel compilations, so they are not wall time. Session
wall time is the shim invocation's duration.
"""
import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
from collections import defaultdict

NAME = re.compile(
    r"^(?P<ts>\d{8}T\d{6}Z)-(?P<pid>\d+)-(?P<mode>[^-]+)-(?P<sub>[^-]+)-(?P<wt>.+)\.json$"
)


def reports_dir():
    try:
        common = subprocess.check_output(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
            text=True, stderr=subprocess.DEVNULL,
        ).strip()
        root = os.path.dirname(common)
    except Exception:
        root = os.getcwd()
    return os.path.join(root, "state", "mbx", "reports")


def parse_since(text):
    match = re.fullmatch(r"(\d+)([hdm])", text)
    if not match:
        sys.exit(f"--since expects e.g. 24h, 7d, 30m; got {text!r}")
    n, unit = int(match.group(1)), match.group(2)
    return dt.timedelta(**{{"h": "hours", "d": "days", "m": "minutes"}[unit]: n})


def load(path):
    with open(path) as handle:
        return json.load(handle)


def ns_to_s(value):
    return (value or 0) / 1e9


def collect(directory, since):
    cutoff = dt.datetime.now(dt.timezone.utc) - since
    rows = []
    for name in sorted(os.listdir(directory)):
        match = NAME.match(name)
        if not match:
            continue
        when = dt.datetime.strptime(match.group("ts"), "%Y%m%dT%H%M%SZ").replace(tzinfo=dt.timezone.utc)
        if when < cutoff:
            continue
        try:
            report = load(os.path.join(directory, name))
        except (OSError, ValueError):
            continue
        bypasses = report.get("bypasses") or {}
        rows.append({
            "when": when,
            "day": when.strftime("%Y-%m-%d"),
            "mode": match.group("mode"),
            "subcommand": match.group("sub"),
            "worktree": match.group("wt"),
            "wall_s": ns_to_s(report.get("session_duration_ns")),
            "hits": report.get("hits", 0),
            "misses": report.get("misses", 0),
            "unconsulted": report.get("unconsulted", 0),
            "bypassed": sum(bypasses.values()) if isinstance(bypasses, dict) else 0,
            "avoided_s": ns_to_s(report.get("estimated_compiler_duration_avoided_ns")),
            "spent_s": ns_to_s(((report.get("compiler") or {}).get("bypass") or {}).get("duration_ns"))
            + sum(ns_to_s(item.get("duration_ns")) for item in report.get("slow_compilations") or []),
            "reflinked_mb": (report.get("reflinked_output_bytes") or 0) / 1e6,
            "slow": report.get("slow_compilations") or [],
        })
    return rows


def summarize(rows):
    total = {
        "builds": len(rows),
        "wall_s": sum(r["wall_s"] for r in rows),
        "hits": sum(r["hits"] for r in rows),
        "misses": sum(r["misses"] for r in rows),
        "unconsulted": sum(r["unconsulted"] for r in rows),
        "bypassed": sum(r["bypassed"] for r in rows),
        "avoided_s": sum(r["avoided_s"] for r in rows),
        "spent_s": sum(r["spent_s"] for r in rows),
        "reflinked_mb": sum(r["reflinked_mb"] for r in rows),
    }
    looked_up = total["hits"] + total["misses"]
    total["hit_rate"] = total["hits"] / looked_up if looked_up else 0.0
    return total


def group(rows, key):
    buckets = defaultdict(list)
    for row in rows:
        buckets[row[key]].append(row)
    return {name: summarize(items) for name, items in sorted(buckets.items())}


def fmt_row(name, s, width):
    return (
        f"{name:<{width}} {s['builds']:>6} {s['hits']:>6} {s['misses']:>6} {s['bypassed']:>7} "
        f"{100 * s['hit_rate']:>6.0f}% {s['avoided_s'] / 60:>8.1f} {s['spent_s'] / 60:>8.1f} {s['wall_s'] / 60:>8.1f}"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--since", default="24h", help="window, e.g. 24h, 7d (default 24h)")
    parser.add_argument("--by", choices=["worktree", "day", "mode", "subcommand"], default="worktree")
    parser.add_argument("--dir", default=reports_dir(), help="reports directory")
    parser.add_argument("--json", action="store_true", help="print totals as JSON")
    parser.add_argument("--top", type=int, default=8, help="slowest compilations to list")
    args = parser.parse_args()

    if not os.path.isdir(args.dir):
        sys.exit(f"no reports at {args.dir}; builds through scripts/bin/cargo write them")
    rows = collect(args.dir, parse_since(args.since))
    total = summarize(rows)
    if args.json:
        print(json.dumps({"since": args.since, "total": total, "by": {args.by: group(rows, args.by)}}, indent=2))
        return
    if not rows:
        print(f"no mbx build reports in the last {args.since} under {args.dir}")
        return

    print(f"mbx build reports, last {args.since}: {total['builds']} builds "
          f"({total['wall_s'] / 60:.1f} min wall), hit rate {100 * total['hit_rate']:.0f}% "
          f"({total['hits']} hits / {total['misses']} misses / {total['unconsulted']} first-seen / {total['bypassed']} bypassed)")
    print(f"compiler time: {total['avoided_s'] / 60:.1f} min avoided (estimated) vs {total['spent_s'] / 60:.1f} min spent; "
          f"{total['reflinked_mb'] / 1000:.2f} GB restored by reflink")
    print()
    width = max(12, *(len(k) for k in group(rows, args.by)))
    print(f"{args.by:<{width}} {'builds':>6} {'hits':>6} {'misses':>6} {'bypass':>7} {'hit%':>7} {'avoid_m':>8} {'spent_m':>8} {'wall_m':>8}")
    for name, s in group(rows, args.by).items():
        print(fmt_row(name, s, width))

    slow = defaultdict(float)
    for row in rows:
        for item in row["slow"]:
            slow[item.get("crate_name", "?")] += ns_to_s(item.get("duration_ns"))
    if slow:
        print()
        print("slowest uncached compilations (summed):")
        for crate, seconds in sorted(slow.items(), key=lambda kv: -kv[1])[: args.top]:
            print(f"  {crate:<40} {seconds:>7.1f} s")


if __name__ == "__main__":
    main()
