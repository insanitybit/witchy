#!/usr/bin/env python3
"""Generated TOML documents, read by Witchy and by Python's tomllib, compared value for value.

    toml-differential.py (--host HOST --module dump.wasm | --native EXE) [--count 5000] [--seed 1]

EXE or the module is tests/toml/dump.wy. Where the corpus has one case of each rule, this makes
many of their combinations: tables named by headers, dotted keys, arrays of tables and inline
tables over a few keys (so that they meet), written plainly, quoted and escaped; and numbers,
dates and strings in every form. A document must be read by both or refused by both, and what
is read must be the same: every key, every value bit for bit, in the order written.
"""
import argparse, re, concurrent.futures, datetime, math, os, random, shutil, struct, subprocess, sys, tempfile, tomllib

p = argparse.ArgumentParser()
p.add_argument('--host'); p.add_argument('--module'); p.add_argument('--native')
p.add_argument('--count', type=int, default=5000); p.add_argument('--seed', type=int, default=1)
p.add_argument('--jobs', type=int, default=8); p.add_argument('--show', type=int, default=8)
a = p.parse_args()
R = random.Random(a.seed)

# tomllib reads TOML 1.1 from Python 3.15 and 1.0 before: both read the version it reads, and
# the forms 1.1 added are made only for 1.1.
try: tomllib.loads('a = {b = 1,}'); MODERN = True
except tomllib.TOMLDecodeError: MODERN = False

def integer():
    n = R.choice([0, 1, -1, 7, 42, 2**63 - 1, -2**63, R.randrange(-10**6, 10**6), R.randrange(-2**63, 2**63)])
    how = R.randrange(6)
    if how == 0 and n >= 0: return R.choice(['0x%x' % n, '0x%X' % n, '0o%o' % n, '0b' + bin(n)[2:]])
    text = str(abs(n))
    if how == 1 and len(text) > 3: text = text[:-3] + '_' + text[-3:]
    return ('-' if n < 0 else R.choice(['', '+'])) + text

def floating():
    how = R.randrange(8)
    if how == 0: return R.choice(['inf', '+inf', '-inf', 'nan', '+nan', '-nan'])
    if how == 1: return repr(struct.unpack('<d', struct.pack('<Q', R.getrandbits(64) & 0x7fefffffffffffff))[0]).replace('e+', R.choice(['e+', 'e', 'E']))
    digits = ''.join(R.choice('0123456789') for _ in range(R.choice([1, 2, 3, 5, 15, 16, 17, 18, 19, 20, 25, 40])))
    whole = str(int(digits[:R.randrange(1, len(digits) + 1)])); rest = digits[len(whole):] if R.random() < 0.7 else ''
    text = whole + ('.' + rest if rest else '')
    if R.random() < 0.6 or not rest: text += R.choice('eE') + R.choice(['', '+', '-']) + str(R.choice([0, 1, 5, 22, 23, 100, 300, 308, 309, 320, 324, 400, R.randrange(330)]))
    return R.choice(['', '+', '-']) + text

def date():
    y, m = R.choice([1, 1979, 2000, 2024, 2100, 9999, R.randrange(1, 9999)]), R.randrange(1, 13)
    leap = y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)
    d = R.randrange(1, [31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][m - 1] + 1)
    return '%04d-%02d-%02d' % (y, m, d)

def clock():
    if MODERN and R.random() < 0.2: return '%02d:%02d' % (R.randrange(24), R.randrange(60))
    text = '%02d:%02d:%02d' % (R.randrange(24), R.randrange(60), R.randrange(60))
    if R.random() < 0.5: text += '.' + ''.join(R.choice('0123456789') for _ in range(R.choice([1, 3, 6])))
    return text

def moment():
    how = R.randrange(4)
    if how == 0: return date()
    if how == 1: return clock()
    text = date() + R.choice('Tt ') + clock()
    if how == 2: return text
    return text + R.choice(['Z', 'z', '%s%02d:%02d' % (R.choice('+-'), R.randrange(24), R.randrange(60))])

PIECES = ['a', 'b', ' ', 'é', '日本', '😀', '\\n', '\\t', '\\\\', '\\"', '\\u00e9', '\\U0001F600', '\\b', '\\f', '\\r', "'", '#', '=', '[', '.']
if MODERN: PIECES += ['\\e', '\\x41', '\\xe9']
def string():
    how = R.randrange(4)
    body = ''.join(R.choice(PIECES) for _ in range(R.randrange(0, 7)))
    if how == 0: return '"' + body + '"'
    plain = body.replace('\\', '')
    if how == 1: return "'" + plain.replace("'", '') + "'"
    if how == 2: return '"""' + R.choice(['', '\n']) + body + R.choice(['', '\n', ' \\\n   ', '""', '"']) + '"""'
    return "'''" + R.choice(['', '\n']) + plain.replace("'''", '') + R.choice(['', "'", "''"]) + "'''"

def scalar(): return R.choice([integer, floating, moment, string, lambda: R.choice(['true', 'false'])])()

NAMES = [['a', 'b', 'c'], ['a', 'b', 'c', '"a"', "'b'", '"\\u0061"', '""', '"c d"', '1', '-']]
def document():
    names = R.choice(NAMES)
    def path(): return R.choice(['.', ' . ', '.']).join(R.choice(names) for _ in range(R.choice([1, 1, 2, 2, 3])))
    def value(depth=0):
        how = R.random()
        if how < 0.55 or depth > 2: return scalar() if R.random() < 0.5 else R.choice(['1', '"s"', 'true'])
        if how < 0.75: return '[' + ', '.join(value(depth + 1) for _ in range(R.randrange(4))) + R.choice(['', ',', '\n']) + ']'
        inside = [path() + ' = ' + value(depth + 1) for _ in range(R.randrange(4))]
        if MODERN and inside and R.random() < 0.3: return '{' + R.choice(['', '\n']) + R.choice([', ', ',\n']).join(inside) + R.choice(['', ',', '\n', ',\n']) + '}'
        return '{' + ', '.join(inside) + '}'
    lines = []
    for _ in range(R.randrange(1, 10)):
        how = R.random()
        if how < 0.3: lines.append('[' + path() + ']')
        elif how < 0.5: lines.append('[[' + path() + ']]')
        else: lines.append(path() + ' = ' + value() + R.choice(['', '', ' # note']))
    return R.choice(['\n', '\r\n']).join(lines) + R.choice(['\n', ''])

def expected(node):
    if isinstance(node, dict): return {key: expected(value) for key, value in node.items()}
    if isinstance(node, list): return [expected(value) for value in node]
    if isinstance(node, bool): return ('b', node)
    if isinstance(node, str): return ('s', node)
    if isinstance(node, int): return ('i', node)
    if isinstance(node, float): return ('f', 'nan') if math.isnan(node) else ('f', struct.unpack('<Q', struct.pack('<d', node))[0])
    if isinstance(node, datetime.datetime):
        zone = node.utcoffset()
        minutes = 0 if zone is None else int(zone.total_seconds()) // 60
        return ('d', 6 if zone is not None else 7, node.year, node.month, node.day, node.hour, node.minute, node.second, node.microsecond, abs(minutes), minutes < 0)
    if isinstance(node, datetime.date): return ('d', 8, node.year, node.month, node.day, 0, 0, 0, 0, 0, False)
    return ('d', 9, 0, 0, 0, node.hour, node.minute, node.second, node.microsecond, 0, False)

def parsed(lines):
    def value(at):
        line = lines[at]
        if line == '{':
            table, at = {}, at + 1
            while lines[at] != '}':
                key = bytes.fromhex(lines[at][2:]).decode()
                if key in table: raise ValueError('a key twice')
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
        # tomllib keeps microseconds: compare at that grain.
        return ('d', *parts[:7], parts[7] // 1000, parts[8], parts[9] == 1 and parts[8] != 0), at + 1
    return value(0)[0]

def same(got, want):
    """Equal, with a table's keys in the same order."""
    if isinstance(want, dict): return isinstance(got, dict) and list(got) == list(want) and all(same(got[key], want[key]) for key in want)
    if isinstance(want, list): return isinstance(got, list) and len(got) == len(want) and all(same(x, y) for x, y in zip(got, want))
    return got == want

def run(text):
    try:
        want = expected(tomllib.loads(text))
        # tomllib reads digits past the largest number as infinity; Witchy, like the `toml`
        # crate, refuses them: more infinities read than `inf` written means one is there.
        shown = repr(want)
        if shown.count("('f', 9218868437227405312)") + shown.count("('f', 18442240474082181120)") > len(re.findall(r'inf', text)): want = None
    except RecursionError: return None
    except Exception: want = None
    work = os.path.realpath(tempfile.mkdtemp(prefix='toml-differential-'))
    try:
        open(os.path.join(work, 'input.toml'), 'w', newline='').write(text)
        if not MODERN: open(os.path.join(work, 'v1.0'), 'w').close()
        command = [a.native, '--output', '--allocator', f'--dir={work}'] if a.native else [a.host, a.module, '--output', '--allocator', '--unmetered', f'--dir={work}']
        done = subprocess.run(command, capture_output=True, timeout=60)
        out = done.stdout.decode(errors='replace')
        if done.returncode != 0: return f'exit {done.returncode}'
        if out.startswith('invalid'): return None if want is None else 'refused, and tomllib reads it'
        if want is None: return 'read, and tomllib refuses it'
        got = parsed(out.split('\n'))
        return None if same(got, want) else f'differs: got {str(got)[:200]} want {str(want)[:200]}'
    except Exception as error: return f'harness: {error!r}'
    finally: shutil.rmtree(work, ignore_errors=True)

texts = [document() for _ in range(a.count)]
with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool: results = list(pool.map(run, texts))
bad = [(text, why) for text, why in zip(texts, results) if why]
for text, why in bad[:a.show]: print('FAIL', why, repr(text))
print(f'{a.count} documents (TOML {"1.1" if MODERN else "1.0"}): {len(bad)} differ')
sys.exit(1 if bad else 0)
