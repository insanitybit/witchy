#!/usr/bin/env python3
"""Mutants of the toml-test corpus, read by Witchy and by the Rust `toml` crate.

    toml-fuzz.py --corpus tests/toml/toml-test --oracle PATH/oracle
                 (--host HOST --module accepts.wasm | --native EXE) [--count 10000] [--seed 1]

Fails on a wrong accept: text Witchy reads that the `toml` crate refuses. Text Witchy refuses
that the crate reads is listed (`--show`), and fails the run only with `--strict`.
The mutants are the same for a given seed.
"""
import argparse, os, random, subprocess, sys, tempfile

p = argparse.ArgumentParser()
p.add_argument('--corpus', required=True); p.add_argument('--oracle', required=True)
p.add_argument('--host'); p.add_argument('--module'); p.add_argument('--native')
p.add_argument('--count', type=int, default=10000); p.add_argument('--seed', type=int, default=1)
p.add_argument('--batch', type=int, default=2000); p.add_argument('--show', type=int, default=10)
p.add_argument('--strict', action='store_true'); p.add_argument('--keep')
a = p.parse_args()

r = random.Random(a.seed)
seeds = []
for root, _, names in os.walk(a.corpus):
    for name in sorted(names):
        if name.endswith('.toml'): seeds.append(open(os.path.join(root, name), 'rb').read())
seeds.sort()
TOKENS = [b'=', b'.', b',', b'[', b']', b'[[', b']]', b'{', b'}', b'"', b"'", b'"""', b"'''", b'\n', b'\r\n', b'\r', b' ', b'\t', b'#',
          b'\\', b'\\n', b'\\u00e9', b'\\x41', b'\\e', b'-', b'+', b'_', b'0', b'1', b'9', b'e', b'E', b'x', b'o', b'b', b':', b'T', b'Z',
          b'inf', b'nan', b'true', b'false', b'a', b'key', b'1979-05-27', b'07:32:00', b'0x', b'1e5', b'\x00', b'\x7f', b'\xc3\xa9', b'\xff']

def mutant():
    data = bytearray(r.choice(seeds))
    for _ in range(r.choice([1, 1, 1, 2, 2, 3, 5])):
        how = r.randrange(7)
        at = r.randrange(len(data) + 1)
        if how == 0 and data: del data[r.randrange(len(data))]
        elif how == 1: data[at:at] = r.choice(TOKENS)
        elif how == 2 and data: data[r.randrange(len(data))] = r.choice(r.choice(TOKENS))
        elif how == 3 and len(data) > 1:
            i = r.randrange(len(data) - 1); data[i], data[i + 1] = data[i + 1], data[i]
        elif how == 4:
            lines = data.split(b'\n'); other = r.choice(seeds).split(b'\n')
            lines.insert(r.randrange(len(lines) + 1), r.choice(other)); data = bytearray(b'\n'.join(lines))
        elif how == 5 and data:
            i = r.randrange(len(data)); j = min(len(data), i + r.randrange(1, 8)); data[at:at] = data[i:j]
        elif how == 6 and data:
            lines = data.split(b'\n'); del lines[r.randrange(len(lines))]; data = bytearray(b'\n'.join(lines))
    return bytes(data)

wrong_accept, wrong_refuse, done, both = [], [], 0, 0
while done < a.count:
    size = min(a.batch, a.count - done)
    work = os.path.realpath(tempfile.mkdtemp(prefix='toml-fuzz-'))
    texts = [mutant() for _ in range(size)]
    for number, text in enumerate(texts): open(os.path.join(work, f'm{number}'), 'wb').write(text)
    open(os.path.join(work, 'count'), 'w').write(str(size))
    command = [a.native, '--output', '--allocator', f'--dir={work}'] if a.native else \
        [a.host, a.module, '--output', '--allocator', '--unmetered', f'--dir={work}']
    ours = subprocess.run(command, capture_output=True, timeout=600)
    theirs = subprocess.run([a.oracle, work, str(size)], capture_output=True, timeout=600)
    mine, crate = ours.stdout.decode().strip(), theirs.stdout.decode().strip()
    if ours.returncode != 0 or len(mine) != size or len(crate) != size:
        print(f'batch failed: exit {ours.returncode}, {len(mine)} and {len(crate)} answers of {size}: {ours.stderr.decode()[:300]}'); sys.exit(2)
    for number, text in enumerate(texts):
        if mine[number] == '1' and crate[number] == '0': wrong_accept.append(text)
        if mine[number] == '0' and crate[number] == '1': wrong_refuse.append(text)
        both += mine[number] == '1' and crate[number] == '1'
    done += size
    if not a.keep:
        for name in os.listdir(work): os.unlink(os.path.join(work, name))
        os.rmdir(work)
for label, found in (('WRONG ACCEPT', wrong_accept), ('refused, the crate reads it', wrong_refuse)):
    for text in found[:a.show]: print(label, repr(text[:160]))
if a.keep:
    os.makedirs(a.keep, exist_ok=True)
    for kind, found in (('accept', wrong_accept), ('refuse', wrong_refuse)):
        for number, text in enumerate(found): open(os.path.join(a.keep, f'{kind}-{number}.toml'), 'wb').write(text)
print(f'{done} mutants: {both} read by both, {len(wrong_accept)} wrong accepts, {len(wrong_refuse)} refused that the crate reads')
sys.exit(1 if wrong_accept or (a.strict and wrong_refuse) else 0)
