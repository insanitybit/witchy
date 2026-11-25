#!/usr/bin/env python3
"""Write a 1 MB TOML file of mixed values, the same bytes on every run: generate.py OUT"""
import random, sys
r = random.Random(20261125)
words = ['alpha', 'bravo', 'charlie', 'delta', 'echo', 'foxtrot', 'golf', 'hotel', 'india', 'juliet', 'kilo', 'lima']
out, size, n = [], 0, 0
def emit(line):
    global size
    out.append(line); size += len(line) + 1
while size < 1_000_000:
    n += 1
    emit(f'[[package]]')
    emit(f'name = "{r.choice(words)}-{r.choice(words)}-{n}"')
    emit(f'version = "{r.randrange(9)}.{r.randrange(40)}.{r.randrange(40)}"')
    emit(f'description = "A {r.choice(words)} of {r.choice(words)} for the {r.choice(words)}, made to be read quickly and without a copy."')
    emit(f'weight = {r.randrange(-10**9, 10**9)}')
    emit(f'ratio = {r.random() * 1000:.6f}')
    emit(f'enabled = {"true" if r.random() < 0.5 else "false"}')
    emit(f'released = 20{r.randrange(10, 27)}-{r.randrange(1, 13):02}-{r.randrange(1, 29):02}T{r.randrange(24):02}:{r.randrange(60):02}:{r.randrange(60):02}Z')
    emit('features = [' + ', '.join(f'"{r.choice(words)}"' for _ in range(r.randrange(1, 8))) + ']')
    emit('limits = { low = %d, high = %d, note = \'%s\' }' % (r.randrange(100), r.randrange(100, 10000), r.choice(words)))
    if n % 7 == 0: emit(f'path = "C:\\\\{r.choice(words)}\\\\{r.choice(words)}\\tend"')
    emit(f'[package.metadata.{r.choice(words)}{n}]')
    emit(f'checksum = "{r.getrandbits(256):064x}"')
    emit(f'dependencies.{r.choice(words)}.version = "{r.randrange(9)}.{r.randrange(9)}"')
    emit('')
open(sys.argv[1], 'w').write('\n'.join(out) + '\n')
