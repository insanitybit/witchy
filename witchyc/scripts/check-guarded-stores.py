#!/usr/bin/env python3
"""Store form against guarded form: an owner assigned over a field is emitted as one store once
its proof is accepted (src/compiler/vacant_store.rs). The operator's audit switch
WITCHY_AUDIT_GUARDED_STORES=1 emits it as the exchange and match the proof was about instead.
This compiles each program both ways, runs both and requires the same output.

  scripts/check-guarded-stores.py --compiler BIN --runtime HOST --binding FILE [--proof-cache DIR] [PROGRAM.wy...]

Without programs: every accepted fixture in tests/fixtures/vacant-assign and
tests/fixtures/owning-helpers and the four-node list client; a fixture named *.traps.wy must
trap at an `unreachable` both ways. The list client and the refill
fixture must also differ between the two builds, or the switch did nothing.
"""
import argparse, os, re, subprocess, sys, tempfile
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--compiler', type=Path, required=True)
p.add_argument('--runtime', type=Path, required=True)
p.add_argument('--binding', type=Path, required=True)
p.add_argument('--proof-cache', type=Path)
p.add_argument('--retries', type=int, default=3, help='compiles again when the proof helper misses its deadline')
p.add_argument('programs', nargs='*', type=Path)
a = p.parse_args()
repo = Path(__file__).resolve().parents[1]
SWITCH = 'WITCHY_AUDIT_GUARDED_STORES'
# The two builds must differ here: these assign an owner over a field in a proved function.
MUST_DIFFER = [repo / 'examples/node-links/composed-forward.wy', repo / 'tests/fixtures/vacant-assign/refill.wy']
programs = a.programs or sorted(
    [f for d in ('vacant-assign', 'owning-helpers') for f in (repo / 'tests/fixtures' / d).glob('*.wy') if not f.name.endswith('.refused.wy')]
    + MUST_DIFFER[:1])
env = {k: v for k, v in os.environ.items() if not k.startswith(('WITCHY_', 'LEAN_', 'LAKE_'))}
env['WITCHY_WASM_READY_BINDING'] = str(a.binding)
if 'WITCHY_WASM_LEAN_ROOT' in os.environ: env['WITCHY_WASM_LEAN_ROOT'] = os.environ['WITCHY_WASM_LEAN_ROOT']
if a.proof_cache:
    a.proof_cache.mkdir(parents=True, exist_ok=True)
    env['WITCHY_PROOF_CACHE'] = str(a.proof_cache.resolve())

def build(program, module, guarded):
    for _ in range(a.retries + 1):
        built = subprocess.run([a.compiler, 'wasm-checked', program, '-o', module],
                               env={**env, SWITCH: '1'} if guarded else env, capture_output=True, text=True, timeout=600)
        if 'native diagnostic deadline' not in built.stderr: break
    return built

failures = 0
with tempfile.TemporaryDirectory(prefix='witchy-guarded-stores-') as scratch:
    for index, program in enumerate(programs):
        asks = re.search(r'fn main\([^)]*Allocator', program.read_text())
        runs = []
        for guarded in (False, True):
            module = Path(scratch) / f'{index}.{"guard" if guarded else "store"}.wasm'
            built = build(program, module, guarded)
            if built.returncode != 0 or not module.exists():
                runs.append(('refused', built.stderr[-600:], b''))
                continue
            ran = subprocess.run([a.runtime, module, '--output', *(['--allocator'] if asks else [])], env=env, capture_output=True, text=True, timeout=60)
            trapped = ran.returncode != 0 and '`unreachable` instruction executed' in ran.stderr
            runs.append(('trap' if trapped else f'exit {ran.returncode}', ran.stdout, module.read_bytes()))
        (how, out, store), (ghow, gout, guard) = runs
        wanted = 'trap' if program.name.endswith('.traps.wy') else 'exit 0'
        problem = None
        if how != wanted or ghow != wanted: problem = f'store build: {how}, guarded build: {ghow}, expected {wanted} of both\n{out if how == "refused" else ""}{gout if ghow == "refused" else ""}'
        elif out != gout: problem = f'outputs differ\nstore:\n{out}guarded:\n{gout}'
        elif program.resolve() in [m.resolve() for m in MUST_DIFFER] and store == guard: problem = 'the two builds are the same module: the audit switch did nothing'
        if problem: failures += 1
        print(f'{"FAIL" if problem else "ok  "} {program.relative_to(repo) if program.is_relative_to(repo) else program}: '
              f'{problem or f"{wanted} both ways, store {len(store)} bytes, guarded {len(guard)} bytes"}')
sys.exit(1 if failures else 0)
