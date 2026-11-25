#!/usr/bin/env python3
"""Keys chosen to share cells of the TOML reader's index are refused quickly, not read slowly.

    toml-flood.py --native ACCEPTS [--keys N]

ACCEPTS is tests/toml/accepts.wy built for the native target. The same number of ordinary keys
must be read; the chosen keys must be refused, in no more than ten times the ordinary time.
"""
import argparse, os, random, shutil, struct, subprocess, sys, tempfile, time

M = 2**64 - 1
K = 11400714819323198485

def key_hash(parent, key):
    h = 14695981039346656037 ^ ((parent * K) & M)
    at = 0
    while len(key) - at >= 8:
        mixed = h ^ struct.unpack_from('<Q', key, at)[0]
        p = mixed * K
        h = (p >> 64) ^ (p & M)
        at += 8
    while at < len(key):
        h = ((h ^ key[at]) * 1099511628211) & M
        at += 1
    p = h * K
    return (p >> 64) ^ (p & M)

def accepts(exe, text):
    work = os.path.realpath(tempfile.mkdtemp(prefix='toml-flood-'))
    try:
        with open(os.path.join(work, 'm0'), 'wb') as out:
            out.write(text)
        with open(os.path.join(work, 'count'), 'w') as out:
            out.write('1')
        start = time.time()
        run = subprocess.run([exe, '--output', '--allocator', f'--dir={work}'], capture_output=True, timeout=600)
        return run.returncode, run.stdout.decode().strip(), time.time() - start
    finally:
        shutil.rmtree(work)

parser = argparse.ArgumentParser()
parser.add_argument('--native', required=True)
parser.add_argument('--keys', type=int, default=20000)
args = parser.parse_args()
cells = 16
while cells < (args.keys + 2) * 2:
    cells <<= 1
rng = random.Random(5)
alphabet = b'abcdefghijklmnopqrstuvwxyz0123456789'
plain, chosen = set(), set()
while len(chosen) < args.keys:
    key = b'k' + bytes(rng.choice(alphabet) for _ in range(11))
    if len(plain) < args.keys:
        plain.add(key)
    if key_hash(0, key) & (cells - 1) < 64:
        chosen.add(key)
code, answer, ordinary = accepts(args.native, b''.join(key + b' = 1\n' for key in plain))
if code != 0 or answer != '1':
    sys.exit(f'ordinary keys were not read: exit {code}, answer {answer!r}')
code, answer, crowded = accepts(args.native, b''.join(key + b' = 1\n' for key in chosen))
if code != 0 or answer != '0':
    sys.exit(f'chosen keys were not refused: exit {code}, answer {answer!r}')
if crowded > 10 * ordinary + 0.5:
    sys.exit(f'chosen keys took {crowded:.2f}s against {ordinary:.2f}s for ordinary keys')
print(f'{args.keys} keys: ordinary read in {ordinary:.2f}s, chosen refused in {crowded:.2f}s')
