#!/usr/bin/env python3
"""Mutate accepted fixtures at random and look for compiler crashes (not refusals).

    python3 scripts/fuzz-mutants.py COMPILER OUT_DIR COUNT [SEED]

Each mutant: one to three of deleting a line, duplicating one, a small textual swap (`&` and
`&mut`, `move`, operators, `let`/`var`, `Ok`/`Err`, quotes and `${...}` braces, ...), a changed number, or a line spliced from
another fixture at the same indentation. It is compiled with `wasm-traps`, which checks and lowers
to IR without proving. A panic, or an exit code other than 0 or 1, is kept in OUT_DIR/crash with
the compiler's output; a run over 20 s is kept as a timeout. Relative imports are made absolute.
"""
import glob, os, random, re, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SWAPS = [('&mut ', '& '), ('& ', '&mut '), ('move ', ''), (' + ', ' - '), (' < ', ' <= '), (' == ', ' != '),
         ('let ', 'var '), ('var ', 'let '), ('Ok(', 'Err('), ('.close()', '.discard()'), ('return ', 'return move '),
         (' && ', ' || '), ('U64', 'U8'), ('[', '[0 + '),
         # Text literals and their `${...}` values: quotes and braces dropped, doubled or escaped.
         ('"', ''), ('}', ''), ('${', '{'), ('${', '$${'), ('"', '\\"'), ('\\"', '"'), ('}"', '"}"'), ('("', '("${"'),
         ('${', '${"x" + '),
         # Decimal literals, element types and references (the paths added 2026-11-19: exponent and
         # method-on-decimal hints, the owner-index message, the array-copy loop, generic ambiguity).
         ('.0', '.0e'), ('.5', '.5.5'), ('.cell(', '['), ('[U64;', '[U8;'), ('Vec.new(&allocator, ', 'Vec.new(')]

def main():
    compiler, out, count = sys.argv[1], sys.argv[2], int(sys.argv[3])
    random.seed(int(sys.argv[4]) if len(sys.argv) > 4 else 1)
    # Each fixture once, by its real path: `fs/root/outside` links back up the tree (the escape
    # fixture), and a recursive glob follows it again and again.
    files = sorted({os.path.realpath(f) for f in glob.glob(ROOT + '/tests/fixtures/**/*.wy', recursive=True)
                    if '.refused' not in f and '.traps' not in f})
    for d in ('mut', 'crash', 'tmp'): os.makedirs(os.path.join(out, d), exist_ok=True)
    env = dict(os.environ, TMPDIR=os.path.join(out, 'tmp'))
    seen, tally, messages, example = set(), {}, {}, {}
    for i in range(count):
        f = random.choice(files)
        here = os.path.dirname(f)
        src = re.sub(r'import "([^"@][^"]*)"', lambda m: 'import "%s"' % os.path.normpath(os.path.join(here, m.group(1))), open(f).read())
        lines = src.split('\n')
        for _ in range(random.randint(1, 3)):
            k, j = random.randrange(4), random.randrange(len(lines))
            if k == 0 and len(lines) > 3: del lines[j]
            elif k == 1: lines.insert(j, lines[random.randrange(len(lines))])
            elif k == 3:
                line = random.choice(open(random.choice(files)).read().split('\n')).strip()
                lines.insert(j, ' ' * (len(lines[j]) - len(lines[j].lstrip())) + line)
            else:
                a, b = random.choice(SWAPS)
                if a in lines[j]: lines[j] = lines[j].replace(a, b, 1)
                else: lines[j] = re.sub(r'\b\d+\b', lambda m: str(random.choice([0, 1, 2, 255, 256, 4294967296, 18446744073709551615])), lines[j], count=1)
        path = os.path.join(out, 'mut', 'm%d.wy' % i)
        open(path, 'w').write('\n'.join(lines))
        try:
            r = subprocess.run([compiler, 'wasm-traps', path], capture_output=True, text=True, timeout=20, env=env)
        except subprocess.TimeoutExpired:
            os.rename(path, os.path.join(out, 'crash', 'timeout%d.wy' % i)); tally['timeout'] = tally.get('timeout', 0) + 1; continue
        kind = 'compiled' if r.returncode == 0 else 'refused'
        if 'panicked' in r.stderr or r.returncode not in (0, 1):
            sig = re.search(r'panicked at ([^\n]*)', r.stderr)
            key = sig.group(1) if sig else 'exit %d' % r.returncode
            kind = 'crash'
            if key not in seen:
                seen.add(key)
                os.rename(path, os.path.join(out, 'crash', 'c%d.wy' % i))
                open(os.path.join(out, 'crash', 'c%d.txt' % i), 'w').write(f + '\n' + r.stderr[-3000:])
                print('CRASH', i, key, flush=True)
                tally[kind] = tally.get(kind, 0) + 1
                continue
        tally[kind] = tally.get(kind, 0) + 1
        if kind == 'refused':
            # The refusal's message, names and types left out, for finding the vaguest common ones.
            m = re.search(r'W\d{4}(?: \d+\.\.\d+| \([^)]*\))?: (.*)', r.stderr)
            if m:
                said = re.sub(r'`[^`]*`', '`_`', m.group(1))[:120]
                messages[said] = messages.get(said, 0) + 1
                if said not in example: example[said] = f
        os.remove(path)
    print(tally, 'distinct crashes:', len(seen), flush=True)
    print('most common refusals (count, message, a fixture mutated to it):')
    for said, n in sorted(messages.items(), key=lambda kv: -kv[1])[:30]:
        print(f'{n:5}  {said}  [{os.path.relpath(example[said], ROOT)}]')

if __name__ == '__main__':
    main()
