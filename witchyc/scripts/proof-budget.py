#!/usr/bin/env python3
"""Report how the standard library's proof size moves.

Each program in tests/proof-budget/ imports one library alone; `wasm-measure` reports how many
goals its proof selects and how many bytes of statement it sends the prover. Both are
deterministic (and reported from the proof cache too), unlike proof time, which they drive: a
proved library addition once put the Vec library's fresh proof up 370-440 ms and passed every
other check. Growth is printed as a review note for the commit (Astra's proof-budget review,
2026-11-19: zero growth is not a commit condition; the fresh-proof time of the bare Vec client is
measured when it matters). `--strict` makes growth fail. A program that cannot be measured is
always a failure (exit 2), never a silent pass, and `--write` records nothing then.
A program whose goals the shared cache did not hold prints `PROVED` with the prover's time: the
cost of the change's cache invalidation, which is not its fresh-proof time.
Usage: proof-budget.py COMPILER [--write] [--strict]
"""
import json, os, subprocess, sys, tempfile, traceback
from pathlib import Path

def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    here = repo / 'tests' / 'proof-budget'
    if len(sys.argv) < 2:
        print('FAIL usage: proof-budget.py COMPILER [--write] [--strict]')
        return 2
    compiler, write, strict = sys.argv[1], '--write' in sys.argv[2:], '--strict' in sys.argv[2:]
    budget = json.loads((here / 'budget.json').read_text())
    unmeasured, grew, measured = 0, 0, {}
    for name in sorted(budget):
        try:
            with tempfile.TemporaryDirectory(prefix='witchy-proof-budget-') as scratch:
                out = subprocess.run([compiler, 'wasm-measure', str(here / name), '-o', os.path.join(scratch, 'm.wasm')],
                                     capture_output=True, text=True, timeout=600)
            if out.returncode != 0:
                raise RuntimeError(f'wasm-measure exited {out.returncode}: {out.stderr[-400:]}')
            text = out.stdout
            report = json.loads(text[text.index('{'):])
            counts = report['phases']['fresh_proof_check']['counts']
            now = {k: int(counts[k]) for k in ('selected_goals', 'statement_bytes')}
            # Cache invalidation, apart from size: with the shared cache a program the change did
            # not touch runs no prover; one that does says how much fresh proof the change cost here
            # (fresh time itself: scripts/proof-fresh-time.py, run with an empty cache).
            spawned = int(report.get('lean_invocations') or 0)
            if spawned:
                prover_ms = int(report['phases']['fresh_proof_check'].get('child_spawn_to_exit', 0)) / 1e6
                print(f'PROVED {name}: {spawned} prover run(s), {prover_ms:.0f} ms (the cache did not hold these goals)')
        except Exception as error:
            print(f'FAIL {name}: not measured ({type(error).__name__}: {error})')
            unmeasured += 1
            continue
        measured[name] = now
        changes = [f'{k} {budget[name][k]} -> {now[k]}' for k in now if now[k] != budget[name][k]]
        if any(now[k] > budget[name][k] for k in now):
            grew += 1
            print(f'GROWTH {name}: {", ".join(changes)} (review note: say what it buys and what the fresh proof costs)')
        elif changes:
            print(f'ok   {name}: {", ".join(changes)} (smaller: lower the record)')
        else:
            print(f'ok   {name}: {now}')
    if unmeasured:
        print(f'FAIL {unmeasured} program(s) not measured; nothing recorded')
        return 2
    if write:
        (here / 'budget.json').write_text(json.dumps(measured, indent=2) + '\n')
        print('recorded tests/proof-budget/budget.json')
    return 1 if (grew and strict and not write) else 0

if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception:
        print('FAIL proof-budget.py stopped:')
        traceback.print_exc(file=sys.stdout)
        sys.exit(2)
