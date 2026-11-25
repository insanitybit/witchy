#!/usr/bin/env python3
"""Run the toml-test corpus through a Witchy dump program (tests/toml/dump.wy).

    toml-test.py --corpus DIR --version 1.0|1.1 (--host HOST --module dump.wasm | --native EXE)

Every valid case must give the values of its tagged JSON; every invalid case must be refused.
Only the class of the result is compared, never a message.
"""
import argparse, concurrent.futures, json, math, os, re, shutil, struct, subprocess, sys, tempfile

p = argparse.ArgumentParser()
p.add_argument('--corpus', required=True)
p.add_argument('--version', default='1.1')
p.add_argument('--host'); p.add_argument('--module'); p.add_argument('--native')
p.add_argument('--jobs', type=int, default=8)
p.add_argument('--show', type=int, default=30)
a = p.parse_args()

DATE = re.compile(r'^(?:(\d{4})-(\d\d)-(\d\d))?(?:[Tt ]?(\d\d):(\d\d)(?::(\d\d)(?:\.(\d+))?)?)?(Z|z|[+-]\d\d:\d\d)?$')
KINDS = {'datetime': 6, 'datetime-local': 7, 'date-local': 8, 'time-local': 9}

def expected(node):
    if isinstance(node, list): return [expected(x) for x in node]
    if 'type' in node and 'value' in node and isinstance(node['value'], str) and len(node) == 2:
        kind, value = node['type'], node['value']
        if kind == 'string': return ('s', value)
        if kind == 'integer': return ('i', int(value))
        if kind == 'bool': return ('b', value == 'true')
        if kind == 'float':
            number = float(value.replace('+', '')) if 'n' in value else float(value)
            return ('f', 'nan') if math.isnan(number) else ('f', struct.unpack('<Q', struct.pack('<d', number))[0])
        m = DATE.match(value)
        y, mo, d, h, mi, s, frac, zone = m.groups()
        nanos = int((frac or '')[:9].ljust(9, '0'))
        offset, west = 0, False
        if zone and zone not in 'Zz':
            offset = int(zone[1:3]) * 60 + int(zone[4:6]); west = zone[0] == '-' and offset != 0
        return ('d', KINDS[kind], int(y or 0), int(mo or 0), int(d or 0), int(h or 0), int(mi or 0), int(s or 0), nanos, offset, west)
    return {key: expected(value) for key, value in node.items()}

def parsed(lines):
    def value(at):
        line = lines[at]
        if line == '{':
            table, at = {}, at + 1
            while lines[at] != '}':
                key = bytes.fromhex(lines[at][2:]).decode()
                table[key], at = value(at + 1)
            return table, at + 1
        if line == '[':
            items, at = [], at + 1
            while lines[at] != ']':
                item, at = value(at); items.append(item)
            return items, at + 1
        tag, rest = line[0], line[2:]
        if tag == 's': return ('s', bytes.fromhex(rest).decode()), at + 1
        if tag == 'i':
            bits = int(rest); return ('i', bits - (1 << 64) if bits >> 63 else bits), at + 1
        if tag == 'b': return ('b', rest == '1'), at + 1
        if tag == 'f':
            bits = int(rest); number = struct.unpack('<d', struct.pack('<Q', bits))[0]
            return (('f', 'nan') if math.isnan(number) else ('f', bits)), at + 1
        parts = [int(x) for x in line.split()[1:]]
        return ('d', *parts[:9], parts[9] == 1 and parts[8] != 0), at + 1
    return value(0)[0]

def run(case):
    work = os.path.realpath(tempfile.mkdtemp(prefix='toml-test-'))
    try:
        shutil.copy(os.path.join(a.corpus, case), os.path.join(work, 'input.toml'))
        if a.version == '1.0': open(os.path.join(work, 'v1.0'), 'w').close()
        command = [a.native, '--output', '--allocator', f'--dir={work}'] if a.native else [a.host, a.module, '--output', '--allocator', '--unmetered', f'--dir={work}']
        done = subprocess.run(command, capture_output=True, timeout=60)
        out = done.stdout.decode(errors='replace')
        if done.returncode != 0: return case, f'exit {done.returncode}: {done.stderr.decode(errors="replace")[:200]}'
        refused = out.startswith('invalid')
        if case.startswith('invalid/'):
            return case, None if refused else 'accepted'
        if refused: return case, 'refused: ' + out.strip()
        want = expected(json.load(open(os.path.join(a.corpus, case[:-5] + '.json'))))
        got = parsed(out.split('\n'))
        return case, None if got == want else f'differs: got {str(got)[:300]} want {str(want)[:300]}'
    except Exception as error:
        return case, f'harness: {error!r}'
    finally:
        shutil.rmtree(work, ignore_errors=True)

cases = [c for c in open(os.path.join(a.corpus, f'files-toml-{a.version}.0')).read().split() if c.endswith('.toml')]
with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool:
    results = list(pool.map(run, cases))
bad = [(c, why) for c, why in results if why]
for c, why in bad[:a.show]: print('FAIL', c, why)
print(f'{len(cases) - len(bad)} of {len(cases)} pass (TOML {a.version})')
sys.exit(1 if bad else 0)
