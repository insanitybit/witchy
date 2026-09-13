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
        PASS [   2.000s] (1/2) fixture::bin first
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
            ],
            env=env,
            text=True,
            capture_output=True,
            check=True,
        )
        output = result.stdout
        assert "nextest    1/2 slowest completed invocation" in output, output
        assert "execution  102s wall" in output, output
        assert "test time  102s summed per-test wall (overlaps; not CPU)" in output, output
        assert "overlap    1.0x summed-test-time / execution wall" in output, output
        assert "90.0s" not in output, output
        assert "50s summed test time above budget" in output, output
        assert "CPU above budget" not in output, output


if __name__ == "__main__":
    main()
