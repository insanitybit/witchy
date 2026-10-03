#!/usr/bin/env python3
"""Focused regression checks for gate-test-cost.py log segmentation."""

import os
from pathlib import Path
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent


def main():
    with tempfile.TemporaryDirectory() as directory:
        log = Path(directory) / "fixture.log"
        log.write_text(
            """\
        TRY 1 FAIL [   2.000s] (1/2) fixture::bin first
[1] tests list progress: 2 test binaries started (stage+90s)
        SLOW [  90.000s] (───────) fixture::bin slow
        PASS [ 100.000s] (2/2) fixture::bin slow
     Summary [ 102.000s] 2 tests run: 2 passed, 0 skipped
        PASS [   1.000s] (1/1) fixture::later tiny
     Summary [   1.000s] 1 test run: 1 passed, 0 skipped
"""
        )
        env = os.environ.copy()
        env["WITCHY_GATE_LOGS"] = directory
        result = subprocess.run(
            [
                "python3",
                str(ROOT / "scripts/gate-test-cost.py"),
                "--top",
                "3",
                "--budget",
                "50",
                "--jobs",
                "2",
            ],
            env=env,
            text=True,
            capture_output=True,
            check=True,
        )
        output = result.stdout
        assert "nextest    1/2 slowest completed invocation" in output, output
        assert "discovery  all 2 binaries observed started by stage+90s (upper bound; overlaps tests)" in output, output
        assert "startup" not in output, output
        assert "execution  102s wall" in output, output
        assert "test time  102s summed per-test wall (overlaps; not CPU)" in output, output
        assert "overlap    1.0x summed-test-time / execution wall" in output, output
        assert "capacity   100.0s minimum at 2 jobs; 50% observed worker occupancy" in output, output
        assert "headroom   2.0s maximum possible scheduling-only reduction" in output, output
        assert "ignores test-group limits" in output, output
        assert "--- final completion tail (2 tests) ---" in output, output
        assert "1 from end" in output and "fixture::bin" in output and "slow" in output, output
        assert "90.0s" not in output, output
        assert "failed tests and focused reproduction selectors" in output, output
        assert "cargo nextest run -p fixture --test bin -E 'test(=first)'" in output, output
        assert "50s summed test time above budget" in output, output
        assert "CPU above budget" not in output, output

        grouped = subprocess.run(
            [
                "python3", str(ROOT / "scripts/gate-test-cost.py"),
                "--log", str(log), "--jobs", "2", "--group-jobs", "1", "--top", "1",
            ],
            env=env, text=True, capture_output=True, check=True,
        ).stdout
        assert "capacity   102.0s minimum at 1 job; 100% observed worker occupancy" in grouped, grouped
        assert "headroom   0.0s maximum possible scheduling-only reduction" in grouped, grouped
        assert "assumes every selected test shares this one group limit" in grouped, grouped

        summary_only = Path(directory) / "summary-only.log"
        summary_only.write_text("     Summary [  12.000s] 2 tests run: 2 passed\n")
        no_gate_dir = env.copy()
        no_gate_dir["WITCHY_GATE_LOGS"] = str(Path(directory) / "no-gate-logs")
        summary = subprocess.run(
            ["python3", str(ROOT / "scripts/gate-test-cost.py"), "--log", str(summary_only)],
            env=no_gate_dir, text=True, capture_output=True, check=True,
        ).stdout
        assert "execution  12s wall" in summary, summary
        assert "per-test   unavailable; rerun with NEXTEST_STATUS_LEVEL=pass" in summary, summary
        rejected = subprocess.run(
            ["python3", str(ROOT / "scripts/gate-test-cost.py"), "--log", str(summary_only),
             "--assert-budget", "2"],
            env=env, text=True, capture_output=True,
        )
        assert rejected.returncode == 1, rejected
        assert "cannot inspect a per-test budget" in rejected.stderr, rejected.stderr

        partial = Path(directory) / "partial-results.log"
        partial.write_text(
            "        FAIL [   1.000s] (1/2) fixture::bin failed\n"
            "     Summary [   3.000s] 2 tests run: 1 passed, 1 failed\n"
        )
        rejected = subprocess.run(
            ["python3", str(ROOT / "scripts/gate-test-cost.py"), "--log", str(partial),
             "--budget", "2"],
            env=env, text=True, capture_output=True,
        )
        assert rejected.returncode == 1, rejected
        assert "cannot inspect a per-test budget" in rejected.stderr, rejected.stderr
        failures = subprocess.run(
            ["python3", str(ROOT / "scripts/gate-test-cost.py"), "--log", str(partial),
             "--failures"],
            env=env, text=True, capture_output=True, check=True,
        ).stdout
        assert "fixture::bin" in failures and "failed" in failures, failures

        scoped = Path(directory) / "newer-scoped.log"
        scoped.write_text(
            """\
check.sh: serialized nextest selection: --test merge_queue
==> [1] tests (workspace) (t+0s)
        PASS [   1.000s] (1/1) fixture::scoped tiny
     Summary [   1.000s] 1 test run: 1 passed, 0 skipped
"""
        )
        workspace_mtime = log.stat().st_mtime
        os.utime(scoped, (workspace_mtime + 10, workspace_mtime + 10))

        newest = subprocess.run(
            ["python3", str(ROOT / "scripts/gate-test-cost.py"), "--top", "1"],
            env=env,
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        assert "gate log   newer-scoped.log" in newest, newest

        preflight = Path(directory) / "newest-preflight.log"
        preflight.write_text(
            """\
==> [1] preflight (tests that red the last attempt) (t+0s)
        PASS [   0.500s] (1/1) fixture::preflight exact_red
     Summary [   0.500s] 1 test run: 1 passed, 0 skipped
"""
        )
        os.utime(preflight, (workspace_mtime + 20, workspace_mtime + 20))

        workspace = subprocess.run(
            [
                "python3",
                str(ROOT / "scripts/gate-test-cost.py"),
                "--workspace-only",
                "--top",
                "1",
            ],
            env=env,
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        assert "gate log   fixture.log" in workspace, workspace
        assert "execution  102s wall" in workspace, workspace

        explicit = subprocess.run(
            [
                "python3",
                str(ROOT / "scripts/gate-test-cost.py"),
                "--log",
                str(preflight),
                "--top",
                "1",
            ],
            env=env,
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        assert "gate log   newest-preflight.log" in explicit, explicit
        assert "execution  0s wall" in explicit, explicit


if __name__ == "__main__":
    main()
