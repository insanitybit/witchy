#!/usr/bin/env python3
"""No function of the TOML package can trap, by the compiler's own trap report.

    toml-traps.py --compiler witchyc [PROGRAM]

Runs `witchyc wasm-traps` on a program that reads TOML (tests/fixtures/toml/read.wy) and fails
if the report names a function of the library: as one that traps, or as the way another does.
The report lists only functions that can trap, so a string read that kept a range test would
be on it.
"""
import argparse, re, subprocess, sys
p = argparse.ArgumentParser(); p.add_argument('--compiler', required=True)
p.add_argument('program', nargs='?', default='tests/fixtures/toml/read.wy')
a = p.parse_args()
report = subprocess.run([a.compiler, 'wasm-traps', a.program], capture_output=True, timeout=900).stdout.decode()
lines = [line for line in report.split('\n') if ':' in line and not line.startswith('native diag')]
if not lines: print('no report'); sys.exit(2)
named = [line for line in lines if re.search(r'\b(Toml|Doc(<\w+>)?)\.', line)]
for line in named: print('TRAPS:', line)
print(f'{len(lines)} functions of the program can trap; {len(named)} of them in or through the TOML library')
sys.exit(1 if named else 0)
