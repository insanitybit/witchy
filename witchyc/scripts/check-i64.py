#!/usr/bin/env python3
"""I64 arithmetic against Python integers.

For each of + - * / % and each pair of values near the edges of I64 (and of the
32-bit and square-root boundaries the emitted checks turn on), the emitted code
must print the exact result when it lies in I64 and trap when it does not or when
the divisor is zero. Division truncates toward zero; the remainder takes the sign
of the dividend.

    scripts/check-i64.py --compiler PATH --binding READY-BINDING --runtime HOST [--work DIR]

About fifteen seconds. One program per operation holds the cases that must not trap;
each case that must trap runs alone.
"""
import argparse, os, subprocess, sys, tempfile

LOW, HIGH = -2**63, 2**63 - 1
EDGES = sorted({v for b in (0, 1, 2, 3, 2**31, 2**32, 3037000499, 3037000500, 2**62, 2**62 + 1, 2**63 - 2, 2**63 - 1, 2**63)
                for v in (b, -b) if LOW <= v <= HIGH})

# The programs import nothing, so no library proof is checked and a build takes milliseconds.
SHOW = '''fn show(console: &Console<Write>, value: I64):
    if value >= 0:
        console.print_int(I64.bits(value))
        return
    console.print_byte(45)
    console.print_int((18446744073709551615 - I64.bits(value)) + 1)
'''

def truncated(a, b):
    q = abs(a) // abs(b)
    return q if (a < 0) == (b < 0) else -q

OPS = {'+': lambda a, b: a + b, '-': lambda a, b: a - b, '*': lambda a, b: a * b,
       '/': lambda a, b: None if b == 0 else truncated(a, b),
       '%': lambda a, b: None if b == 0 else a - b * truncated(a, b)}

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--compiler', required=True); p.add_argument('--binding', required=True)
    p.add_argument('--runtime', required=True); p.add_argument('--work')
    a = p.parse_args()
    work = a.work or tempfile.mkdtemp(prefix='witchy-i64-')
    os.makedirs(work, exist_ok=True)
    env = dict(os.environ, WITCHY_WASM_READY_BINDING=a.binding)

    def run(name, op, pairs):
        source, module = os.path.join(work, name + '.wy'), os.path.join(work, name + '.wasm')
        lines = ''.join(f'    show(&console, apply({x}, {y}))\n' for x, y in pairs)
        with open(source, 'w') as f:
            f.write(f'{SHOW}\nfn apply(a: I64, b: I64) -> I64:\n    return a {op} b\n\nfn main(console: Console<Write>):\n{lines}')
        built = subprocess.run([a.compiler, 'wasm-checked', source, '-o', module], env=env, capture_output=True, text=True)
        if built.returncode != 0:
            sys.exit(f'{name}: did not build\n{built.stderr[:600]}')
        with open(module, 'rb') as f:
            ran = subprocess.run([a.runtime, '--output', '--allocator'], stdin=f, capture_output=True, text=True)
        return ran.returncode, ran.stdout.split()

    exact = traps = 0; bad = []
    for index, (op, fn) in enumerate(OPS.items()):
        fine, must_trap = [], []
        for x in EDGES:
            for y in EDGES:
                r = fn(x, y)
                (fine if r is not None and LOW <= r <= HIGH else must_trap).append((x, y, r))
        code, out = run(f'fine{index}', op, [(x, y) for x, y, _ in fine])
        if code != 0 or len(out) != len(fine):
            bad.append(f'{op}: the in-range cases stopped after {len(out)} of {len(fine)}')
        for (x, y, r), got in zip(fine, out):
            if got == str(r): exact += 1
            else: bad.append(f'{x} {op} {y}: {got}, not {r}')
        for n, (x, y, r) in enumerate(must_trap):
            code, out = run(f'trap{index}', op, [(x, y)])
            if code != 0 and not out: traps += 1
            else: bad.append(f'{x} {op} {y}: no trap, printed {out}')
    print(f'{exact} exact, {traps} trapped, {len(bad)} wrong')
    for line in bad[:20]: print('  ' + line)
    sys.exit(1 if bad else 0)

main()
