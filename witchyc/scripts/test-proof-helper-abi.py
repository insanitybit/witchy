#!/usr/bin/env python3
"""Exercise Lean helper ABIs with real accepted and rejected proofs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--compiler', type=Path, required=True)
parser.add_argument('--plain-helper', type=Path, required=True)
parser.add_argument('--sdk', type=Path, required=True)
parser.add_argument('--binding', type=Path, action='append', required=True)
args = parser.parse_args()
repo = Path(__file__).resolve().parents[1]

imports = [
    'Lean.Elab.BuiltinCommand', 'Lean.Elab.BuiltinTerm', 'Lean.Elab.Declaration',
    'Lean.Elab.ElabRules', 'Lean.Elab.BuiltinEvalCommand', 'Lean.Elab.StructInst',
    'Lean.Elab.Tactic.Omega', 'Lean.Elab.Tactic.Match', 'Lean.Elab.Tactic.Simp',
    'Lean.Elab.Tactic.Unfold', 'Lean.Util.CollectAxioms', 'Init.Omega',
]
header = ''.join(f'import {name}\n' for name in imports)
binding, nonce = 'abi-test', '0123456789abcdef0123456789abcdef'

def frame(**values):
    return json.dumps(dict(binding=binding, nonce=nonce, **values)) + '\n'

for valid in (True, False):
    claim = '1 = 1' if valid else '1 = 2'
    body = header + f'theorem claim : {claim} := by decide\n#eval IO.println "abi-marker"\n'
    request = (frame(op='LOAD', seq=0) +
               frame(op='CHECK_DIAG', seq=1, bytes=len(body.encode()), marker='abi-marker', claim='claim') + body)
    if valid:
        request += frame(op='STOP', seq=2)
    result = subprocess.run([args.plain_helper, args.sdk, binding, nonce], input=request,
                            text=True, capture_output=True, timeout=60)
    expected = 'ok' if valid else 'error'
    if result.returncode != (0 if valid else 1) or f'"status":"{expected}"' not in result.stdout or result.stderr:
        raise RuntimeError(f'plain helper {expected} check failed: {result.stderr}\n{result.stdout}')

fixture = (repo / 'tests/fixtures/ready-vec.wy').read_text()
with tempfile.TemporaryDirectory(prefix='witchy-helper-abi-') as directory:
    root = Path(directory)
    source, output = root / 'program.wy', root / 'program.wasm'
    for selected in args.binding:
        env = dict(os.environ, WITCHY_WASM_READY_BINDING=str(selected.resolve()))
        source.write_text(fixture)
        valid = subprocess.run([args.compiler, 'wasm-checked', source, '-o', output],
                               env=env, capture_output=True, timeout=60)
        if valid.returncode != 0:
            raise RuntimeError(f'{selected}: valid proof refused: {valid.stderr.decode(errors="replace")}')
        before = hashlib.sha256(output.read_bytes()).digest()
        source.write_text('lemma invalid(x: U64):\n    ensures x + 1 < x\n    proof:\n        auto\n' + fixture)
        invalid = subprocess.run([args.compiler, 'wasm-checked', source, '-o', output],
                                 env=env, capture_output=True, timeout=60)
        if invalid.returncode == 0 or b'proof failed' not in invalid.stderr:
            raise RuntimeError(f'{selected}: false proof was not rejected by Lean')
        if hashlib.sha256(output.read_bytes()).digest() != before:
            raise RuntimeError(f'{selected}: false proof changed the output')
        print(f'{selected}: valid proof accepted, false proof refused, output preserved')

print('plain helper: valid proof accepted and false proof refused')
