#!/usr/bin/env python3
"""How many allocations one TOML parse makes, on inputs of 100 KB, 1 MB and 8 MB.

    toml-allocations.py --bench EXE --generate benchmarks/toml/generate.py

EXE is benchmarks/toml/bench.wy built for the native target. Run with
WITCHY_COUNT_ALLOCATIONS set, the native runtime says how many allocations the program asked
for; the difference between one parse and two is what a parse costs (the benchmark's own copy
of the text among them). Fails unless that is the same small number at every size.
"""
import argparse, os, re, subprocess, sys, tempfile
p = argparse.ArgumentParser(); p.add_argument('--bench', required=True); p.add_argument('--generate', required=True)
p.add_argument('--most', type=int, default=8)
a = p.parse_args()
source = open(a.generate).read()
assert source.count('1_000_000') == 1
per_parse = {}
for size in (100_000, 1_000_000, 8_000_000):
    work = os.path.realpath(tempfile.mkdtemp(prefix='toml-allocations-'))
    script = os.path.join(work, 'generate.py')
    open(script, 'w').write(source.replace('1_000_000', str(size)))
    subprocess.run([sys.executable, script, os.path.join(work, 'input.toml')], check=True)
    counts = []
    for parses in (1, 2, 3):
        open(os.path.join(work, 'count'), 'w').write(str(parses))
        run = subprocess.run([a.bench, '--output', '--allocator', f'--dir={work}'], capture_output=True, timeout=600,
                             env={**os.environ, 'WITCHY_COUNT_ALLOCATIONS': '1'})
        found = re.search(r'allocations: (\d+)', run.stdout.decode())
        if run.returncode != 0 or not found: print(f'run failed: exit {run.returncode}'); sys.exit(2)
        counts.append(int(found.group(1)))
    for name in os.listdir(work): os.unlink(os.path.join(work, name))
    os.rmdir(work)
    if counts[1] - counts[0] != counts[2] - counts[1]: print(f'{size}: uneven counts {counts}'); sys.exit(1)
    per_parse[size] = counts[1] - counts[0]
    print(f'{size} bytes: {per_parse[size]} allocations a parse')
same = len(set(per_parse.values())) == 1 and max(per_parse.values()) <= a.most
print('ok' if same else 'FAIL: the number of allocations depends on the size of the input')
sys.exit(0 if same else 1)
