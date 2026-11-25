#!/usr/bin/env python3
"""The fresh (cold) proof time of each tests/proof-budget/ program: compiled with an empty proof
cache, so every selected goal goes to the prover, as on a machine that has not proved this library
yet. Reported separately from cache invalidation (proof-budget.py's `PROVED` lines) and from proof
size (its budget), as decided 2026-11-19: the two-second cold Vec proof is a target, not a gate.
Prints, per program, the median over RUNS of the prover's time and of the whole build.
Usage: proof-fresh-time.py COMPILER [RUNS] [PROGRAM...]
"""
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def main() -> int:
    if len(sys.argv) < 2:
        print('usage: proof-fresh-time.py COMPILER [RUNS] [PROGRAM...]')
        return 2
    compiler = sys.argv[1]
    runs = int(sys.argv[2]) if len(sys.argv) > 2 and sys.argv[2].isdigit() else 3
    here = Path(__file__).resolve().parent.parent / 'tests' / 'proof-budget'
    wanted = [a for a in sys.argv[2:] if not a.isdigit()]
    programs = sorted(here.glob('*.wy')) if not wanted else [here / w for w in wanted]
    for program in programs:
        prover, wall, goals = [], [], 0
        for _ in range(runs):
            with tempfile.TemporaryDirectory(prefix='witchy-fresh-proof-') as cache, tempfile.TemporaryDirectory(prefix='witchy-fresh-out-') as out:
                # By its real path: the compiler refuses a cache directory reached through a link (`/var` on macOS).
                env = dict(os.environ, WITCHY_PROOF_CACHE=os.path.realpath(cache))
                started = time.monotonic()
                done = subprocess.run([compiler, 'wasm-measure', str(program), '-o', os.path.join(out, 'm.wasm')], capture_output=True, text=True, env=env, timeout=1800)
                wall.append(time.monotonic() - started)
                if done.returncode != 0:
                    print(f'FAIL {program.name}: wasm-measure exited {done.returncode}: {done.stderr[-300:]}')
                    return 2
                report = json.loads(done.stdout[done.stdout.index('{'):])
                phase = report['phases']['fresh_proof_check']
                prover.append(int(phase.get('child_spawn_to_exit', 0)) / 1e9)
                goals = int(phase['counts']['selected_goals'])
        print(f'{program.name}: {goals} goals, prover {statistics.median(prover):.2f} s, build {statistics.median(wall):.2f} s (median of {runs}, empty cache)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
