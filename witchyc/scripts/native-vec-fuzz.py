#!/usr/bin/env python3
"""Random Vec operations against a model written apart from the library.

    scripts/native-vec-fuzz.py --compiler target/release/witchyc --binding ready-binding.txt
        [--runtime witchy-wasmtime-host] [--guard] [--proof-cache DIR] [--seeds 200] [--operations 400]

tests/fuzz/vec-owners.wy reads operations from its input, three bytes each, and applies them to
a Vec<U64> and a Vec<String> (each String an allocation of its own): push, pop, read, insert,
remove, swap, truncate, extend from another Vec, replace, reverse, an allocation too large to
grant, a Vec moved through a function by value, and release with a new Vec after it. After each
it prints both vectors, then copies the numbers into a new Vec through two borrows (`mirror`)
and rewrites that one through a reborrow (`settle`), and prints what both read back. With
--expect-regions the builds have alias regions on; the default build must give those two
functions regions, keep no call in either, and have fewer loads in `mirror` than a build
without regions (a load forwarded across the other vector's store). The model then checks that
`settle`'s read after its write saw the write. This script holds Python lists doing the same and compares every line.

The program is built for macos-arm64 three ways (default, WITCHY_NATIVE_CHECKED=1,
WITCHY_NATIVE_OBSERVED=1) and each is run with the heap audit on and must also say that no
allocation is left when it returns (`live bytes: 0`): an owner never released is counted. With
--guard the default build also runs under the system's guard allocator (libgmalloc), which puts
each block against an unmapped page and unmaps it when freed. Every build names a vector's
buffer by slot and generation, so a released one traps before it is read; the guard is a second
line that catches a read past a block or of an outgrown one, which otherwise usually finds old
bytes and still matches. With --runtime the Wasm build runs the same inputs (output only; it has no live count).

What this does not reach: the vectors stay short (tens of elements), the only allocation that
fails is one too large to describe, and nested vectors, records holding owners and tasks are
not in the program.
Exit status 1 if any run differs from the model, fails or times out.
"""
import argparse, os, random, subprocess, sys, tempfile
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
M = 1000000007

def model(data):
    numbers, owners, out = [], [], []
    say = out.append
    say('0 0')  # the shadow of the empty vector, printed before any operation
    for at in range(0, len(data) - len(data) % 3, 3):
        op, a, b = data[at] % 16, data[at + 1], data[at + 2]
        if op == 0: numbers.append(a * 256 + b)
        elif op == 1: say(f'{numbers.pop()} popped' if numbers else 'empty')
        elif op == 2: say(f'{numbers[a]} read' if a < len(numbers) else 'past')
        elif op == 3:
            if a % 8 <= len(numbers): numbers.insert(a % 8, b)
            else: say('not inserted')
        elif op == 4: say(f'{numbers.pop(a % 8)} removed' if a % 8 < len(numbers) else 'none')
        elif op == 5:
            i, j = a % 8, b % 8
            if i < len(numbers) and j < len(numbers): numbers[i], numbers[j] = numbers[j], numbers[i]
            else: say('not swapped')
        elif op == 6:
            if a % 8 <= len(numbers): del numbers[a % 8:]
        elif op == 7: numbers.extend([b] * (a % 5))
        elif op == 8: owners.append(f'w{a * 256 + b}')
        elif op == 9: say(f'{owners.pop(a % 8)} removed' if a % 8 < len(owners) else 'none')
        elif op == 10:
            if a % 8 < len(owners):
                say(f'{owners[a % 8]} replaced'); owners[a % 8] = f'w{70000 + b}'
            else: say('not replaced')
        elif op == 11:
            if a % 8 <= len(owners): owners.insert(a % 8, f'w{80000 + b}')
            else: say(f'w{80000 + b} not inserted')
        elif op == 12: owners.reverse()
        elif op == 13: say(f'{owners.pop()} popped' if owners else 'empty')
        elif op == 14: say('refused'); say('not reserved')
        else:
            if a % 8 <= len(numbers): del numbers[a % 8:]
            if not numbers: say('released')
        total = 0
        for value in numbers: total = (total * 31 + value) % M
        say(' '.join([str(len(numbers)), str(total), str(len(owners)), *owners]))
        mirrored, shadow = 0, []
        for i, value in enumerate(numbers):
            shadow.append(value % 1000 + i)
            mirrored = (mirrored * 31 + value + shadow[i]) % M
        settled = 0
        for i in range(len(shadow)):
            shadow[i] = shadow[i] * 3 % 1000003
            settled = (settled * 31 + shadow[i]) % M
        say(f'{mirrored} {settled}')
    return '\n'.join(out) + '\n' if out else ''

parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument('--compiler', required=True, type=Path)
parser.add_argument('--binding', required=True)
parser.add_argument('--runtime', type=Path)
parser.add_argument('--proof-cache')
parser.add_argument('--guard', action='store_true', help='also run the default build under libgmalloc')
parser.add_argument('--seeds', type=int, default=200)
parser.add_argument('--operations', type=int, default=400)
parser.add_argument('--first-seed', type=int, default=1)
parser.add_argument('--expect-regions', action='store_true',
                    help='build with WITCHY_NATIVE_ALIAS=1 and fail unless mirror and settle get alias regions in the default build')
a = parser.parse_args()
base = {**os.environ, 'WITCHY_WASM_READY_BINDING': a.binding}
if a.expect_regions:
    base.update(WITCHY_NATIVE_ALIAS='1', WITCHY_NATIVE_ALIAS_REPORT='1')
for name in ('WITCHY_NATIVE_CHECKED', 'WITCHY_NATIVE_OBSERVED', 'WITCHY_NATIVE_RUNTIME_CELLS'):
    base.pop(name, None)
if a.proof_cache: base['WITCHY_PROOF_CACHE'] = a.proof_cache
source = repo / 'tests/fuzz/vec-owners.wy'


def machine(exe, function):
    """(instructions, loads, calls) of `function` in `exe`, built with WITCHY_NATIVE_NAMES=1."""
    listing = subprocess.run(['otool', '-tvV', exe], capture_output=True, text=True, check=True).stdout.splitlines()
    body, inside = [], False
    for line in listing:
        if line.startswith('_'):
            inside = line.rstrip(':').split('_', 4)[-1] == function and line.startswith('_witchy_function_')
            continue
        if inside and '\t' in line:
            body.append(line.split('\t')[1])
    return len(body), sum(op.startswith('ld') for op in body), sum(op in ('bl', 'blr') for op in body)


def forwarding(work, with_regions):
    """The default build with regions against one without: `mirror` and `settle` keep no call (so
    their accessors were inlined and their loops are what the regions act on), and `mirror`
    has fewer loads with regions (Cranelift forwarded loads across the store to the other
    vector). `settle`'s read after its write is checked by the model: a forwarded read would give
    the value from before the write."""
    named = {**base, 'WITCHY_NATIVE_NAMES': '1'}
    without = work / 'default-no-regions'
    plain = {k: v for k, v in named.items() if not k.startswith('WITCHY_NATIVE_ALIAS')}
    for exe, env in ((with_regions.with_name('default-named'), named), (without, plain)):
        built = subprocess.run([a.compiler, 'build', source, '-o', exe, '--target=macos-arm64'], env=env, capture_output=True, text=True)
        if built.returncode: sys.exit(f'a named build failed:\n{built.stderr[-2000:]}')
    counts = {(f, on): machine(exe, f) for f in ('mirror', 'settle') for on, exe in ((True, with_regions.with_name('default-named')), (False, without))}
    for f in ('mirror', 'settle'):
        (i1, l1, c1), (i0, l0, c0) = counts[f, True], counts[f, False]
        print(f'  {f}: {i0} instructions, {l0} loads without regions; {i1} instructions, {l1} loads with them; calls {c1}')
        if i1 == 0 or c1:
            sys.exit(f'{f} is missing or still calls: its loop is not what the regions act on')
    if not counts['mirror', True][1] < counts['mirror', False][1]:
        sys.exit('mirror has no fewer loads with regions: no load was forwarded')
failed = False
with tempfile.TemporaryDirectory(prefix='witchy-vec-fuzz-') as work:
    work = Path(work).resolve()
    ways = []
    for name, extra in (('default', {}), ('checked', {'WITCHY_NATIVE_CHECKED': '1'}), ('observed', {'WITCHY_NATIVE_OBSERVED': '1'})):
        exe = work / name
        built = subprocess.run([a.compiler, 'build', source, '-o', exe, '--target=macos-arm64'], env={**base, **extra}, capture_output=True, text=True)
        if built.returncode: sys.exit(f'the {name} build failed:\n{built.stderr[-2000:]}')
        if a.expect_regions:
            # Checked and observed builds reach buffers through runtime calls, which the pass
            # does not classify: only the default build must have regions.
            report = dict(line.split('\t')[1:] for line in built.stderr.splitlines() if line.startswith('alias regions\t'))
            outcomes = {function: report.get(function, 'not reported') for function in ('mirror', 'settle')}
            print(f'{name}: ' + ', '.join(f'{f} {o}' for f, o in outcomes.items()))
            if name == 'default':
                if not all(o.startswith('Ok(') and o != 'Ok(0)' for o in outcomes.values()):
                    sys.exit('the default build gave mirror or settle no alias regions')
                forwarding(work, exe)
        ways.append((name, [exe, '--output', '--input', '--allocator'], {}))
    if a.guard:
        guard = '/usr/lib/libgmalloc.dylib'
        if not os.path.exists(guard): sys.exit(f'--guard needs {guard}')
        ways.append(('default under the guard allocator', [work / 'default', '--output', '--input', '--allocator'], {'DYLD_INSERT_LIBRARIES': guard}))
    if a.runtime:
        module = work / 'module.wasm'
        built = subprocess.run([a.compiler, 'build', source, '-o', module, '--heap-pages=256', '--slots=4096'], env=base, capture_output=True, text=True)
        if built.returncode: sys.exit(f'the Wasm build failed:\n{built.stderr[-2000:]}')
    run_env = {**base, 'WITCHY_HEAP_AUDIT': '1', 'WITCHY_REPORT_LIVE': '1'}
    runs = 0
    for seed in range(a.first_seed, a.first_seed + a.seeds):
        rng = random.Random(seed)
        # Some seeds lean on one kind of operation, so vectors grow long or stay near empty.
        weights = [rng.choice((1, 1, 1, 4)) for _ in range(16)]
        data = bytes(byte for _ in range(a.operations) for byte in (rng.choices(range(16), weights)[0], rng.randrange(256), rng.randrange(256)))
        expected = model(data)
        for name, command, extra in ways:
            try:
                ran = subprocess.run(command, input=data, env={**run_env, **extra}, capture_output=True, timeout=300)
            except subprocess.TimeoutExpired:
                print(f'TIMEOUT seed {seed} {name}'); failed = True; continue
            runs += 1
            want = expected + 'live bytes:  0\n'
            if ran.returncode != 0 or ran.stdout.decode(errors='replace') != want:
                failed = True
                got = ran.stdout.decode(errors='replace').splitlines()
                line = next((i for i, (x, y) in enumerate(zip(got, want.splitlines())) if x != y), min(len(got), len(want.splitlines())))
                print(f'FAIL seed {seed} {name}: status {ran.returncode}, first difference at line {line + 1}: '
                      f'got {got[line] if line < len(got) else None!r}, model {want.splitlines()[line] if line < len(want.splitlines()) else None!r}')
        if a.runtime:
            file = work / 'input'
            file.write_bytes(data)
            ran = subprocess.run([a.runtime, module, '--output', '--output-bytes=16777216', '--allocator', '--unmetered', '--memory-pages=300', f'--input={file}'], env=base, capture_output=True, timeout=120)
            runs += 1
            if ran.returncode != 0 or ran.stdout.decode(errors='replace') != expected:
                failed = True
                print(f'FAIL seed {seed} wasm: status {ran.returncode} {ran.stderr[-200:]!r}')
    print(f'{a.seeds} seeds of {a.operations} operations, {runs} runs: ' + ('FAILED' if failed else 'every run matched the model and left no allocation'))
sys.exit(1 if failed else 0)
