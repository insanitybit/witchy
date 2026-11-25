#!/usr/bin/env python3
"""Differential test of two compilers on random programs: U64 arithmetic with constant
multipliers, small helper functions (written in by the inliner), branches, a loop, values that
overflow. Each program is built by both compilers and run; the output, or the trap's message,
must be the same. For a change to code generation: build the compiler before and after.

  python3 scripts/diff-compilers.py SEED COUNT OLD_COMPILER NEW_COMPILER HOST OUT_DIR [u64|i64|mixed]

`i64` programs use signed arithmetic (its own emitter path); `mixed` alternates by program;
`vec` programs also build a Vec<U64> on the heap, read it back by index and change it in place;
`text` programs build a String from literals and scalars (ASCII and multi-byte) and measure,
compare, replace and walk it; `format` programs print through several interpolations; `maps` programs run random HashMap
operations, each checked against a Vec shadow in the program, with keys hashed to collide and wrap; `vecs` programs change a Vec
(bulk copies, insert, remove, reverse, truncate, swap) and a shadow changed an element at a time;
`keymaps` programs are `maps` programs over a KeyMap under a random hash policy; `derived`
programs derive Eq, Hash and HashKey for random records by recipe and check them; `containers`
programs run Deque and Heap operations against Vec shadows and check a sort; `generators`
programs read a random `gen fn` in place and stepped and compare; `crossgen` programs do so
for another file's generator, beside same-named helpers and a different util.wy;
`strings` programs change an ASCII String (`push_string`, `push_part`, `cut`, ...) against byte values; `records` programs build records, put them in a sum, match on it and
pass records to helpers (field offsets, tags, match arms).

The compilers run with the caller's environment (WITCHY_WASM_READY_BINDING, WITCHY_PROOF_CACHE).
Prints the first few differences and a count; exits 1 if any, or if both compilers refused
more than half of the programs (a generator the library has left behind). `DIFF_COMPILERS_JOBS`
sets how many programs are built at once (8). The self-checking modes (`maps`,
`keymaps`, `vecs`, `strings`, `derived`) also fail when the new build's first line, the program's
count of mismatches, is not 0."""
import random, re, sys, os, subprocess, hashlib, concurrent.futures as cf
seed=int(sys.argv[1]); n=int(sys.argv[2]); old,new,host,out=sys.argv[3:7]
mode=sys.argv[7] if len(sys.argv) > 7 else 'u64'
signed=False
env=dict(os.environ)
def expr(r, names, d):
    if d==0 or r.random()<0.3:
        if names and r.random()<0.6: return r.choice(names)
        if signed: return r.choice(['0','1','3','31','1000','4294967296','(0 - 7)','(0 - 1000)','(0 - 4294967296)','1317624576693539401'])
        # A vec program keeps to small constants, so most reach the heap part before any overflow.
        if mode in ('vec', 'text', 'records', 'generators', 'crossgen'): return str(r.choice([0,1,2,3,7,10,31,255,1000]))
        return str(r.choice([0,1,2,3,7,10,31,255,1000,65536,4294967296,18446744073709551615//7]))
    op=r.choice(['+','-','*','*','/','%'])
    # Unsigned `-` below zero was what stopped most vec programs before their heap part.
    if mode in ('vec', 'text', 'records', 'generators', 'crossgen') and op == '-' and r.random() < (1.0 if mode in ('generators', 'crossgen') else 0.85): op = '+'
    a=expr(r,names,d-1); b=expr(r,names,d-1)
    if op in '/%': b=f'({b} + 1)'  if r.random()<0.5 else str(r.choice([1,3,7,10]))
    if op=='*' and r.random()<0.6: b=str(r.choice([2,3,10,31,1000,4294967296]))
    return f'({a} {op} {b})'
KEYS = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'tests', 'fixtures', 'derive', 'parts', 'keys.wy')

def derived_program(r):
    """A record type with Eq, Hash and HashKey derived by the library-style recipes in
    tests/fixtures/derive/parts/keys.wy: the derived `eq` against a field-by-field comparison,
    equal values' hashes under every policy and the legacy hash, and the type as KeyMap keys."""
    kinds = [r.choice(['U8', 'U16', 'U32', 'U64', 'Bool']) for _ in range(r.randint(1, 5))]
    # Value `i` takes field j from digit j of i in base 3 (Bools from base 2), so values repeat.
    bases = [2 if k == 'Bool' else 3 for k in kinds]
    count = 1
    for b in bases: count *= b
    values = min(count + r.randint(0, 6), 40)
    def field(j, kind, i):
        place = 1
        for b in bases[:j]: place *= b
        digit = f'({i} / {place} % {bases[j]})'
        offset = r.choice([0, 1, 250]) if kind == 'U8' else r.choice([0, 7, 1 << 33]) if kind == 'U64' else r.choice([0, 9])
        if kind == 'Bool': return f'{digit} == 0'
        if kind == 'U64': return f'{digit} * {offset + 1}'
        return f'{kind}.truncate({digit} * {offset + 1})'
    names = [f'f{j}' for j in range(len(kinds))]
    lines = ['import console', 'import hash', 'import keymap', f'import "{os.path.normpath(KEYS)}"', '',
             'type K derive(keys.Key, keys.Fed):'] + [f'    {n}: {t}' for n, t in zip(names, kinds)] + ['',
             'fn make(i: U64) -> K:',
             '    return K(' + ', '.join(f'{n}: {field(j, t, "i")}' for j, (n, t) in enumerate(zip(names, kinds))) + ')', '',
             'fn same(i: U64, j: U64) -> Bool:']
    for j, b in enumerate(bases):
        place = 1
        for c in bases[:j]: place *= c
        lines += [f'    if i / {place} % {b} != j / {place} % {b}:', '        return false']
    lines += ['    return true', '',
              'fn main(console: Console<Write>, allocator: Allocator):',
              '    var bad: U64 = 0',
              f'    let seed: U64 = {r.randrange(1 << 40)}',
              f'    for i in 0..{values}:',
              f'        for j in 0..{values}:',
              '            let a = make(i)', '            let b = make(j)',
              '            if a.eq(&b) != same(i, j):', '                bad += 1',
              '            if same(i, j):',
              '                if a.hash() != b.hash():', '                    bad += 1',
              '                if hash_key(&a, Reference(seed)) != hash_key(&b, Reference(seed)):', '                    bad += 1',
              '                if hash_key(&a, Fast(seed)) != hash_key(&b, Fast(seed)):', '                    bad += 1',
              '                if hash_key(&a, Constant) != hash_key(&b, Constant):', '                    bad += 1',
              f'    let Ok(made): KeyMap<K, U64> = KeyMap.new(&allocator, {r.choice([0, 1, 4, 16])}, {r.choice(["Reference(seed)", "Fast(seed)", "Constant"])}) else Err(e):',
              '        return', '    var map = move made',
              f'    for i in 0..{values}:',
              '        var first = true', '        for j in 0..i:', '            if same(i, j):', '                first = false',
              '        match map.put(&allocator, make(i), i):',
              '            Ok:', '                if !first:', '                    bad += 1',
              '            Err(back):', '                if first:', '                    bad += 1',
              f'    for i in 0..{values}:',
              '        var owner = i', '        var j: U64 = 0', '        while j < i:', '            if same(i, j):', '                owner = j', '                j = i', '            j += 1',
              '        match map.get(&make(i)):', '            Ok(found):', '                if *found != owner:', '                    bad += 1',
              '            Err(code):', '                bad += 1',
              '    console.print_int(bad)', '    console.print_int(map.len())',
              '    var cursor: U64 = 0', '    var going = true',
              '    while going:', '        match map.take_next(&mut cursor):', '            Ok(entry):', '                ()', '            Err(code):', '                going = false',
              '    map.release()']
    return '\n'.join(lines) + '\n'

def keymaps_program(r):
    """`maps_program` over a KeyMap: the key feeds its `id` (which `eq` compares, so the key law
    admits it) and the map hashes under a random policy; `Constant` makes every key collide."""
    text = maps_program(r, upsert=False)
    policy = r.choice(['Reference(%d)' % r.randrange(1000), 'Fast(%d)' % r.randrange(1 << 40), 'Constant'])
    head, body = text.split('fn main(', 1)
    head = re.sub(r'impl Hash for Key:\n    fn hash\(self: &Key\) -> U64:\n        return [^\n]*\n',
                  'impl HashKey for Key:\n    fn feed(self: &Key, state: &mut HashState):\n        (*self).id.feed(state)\n', head)
    head = head.replace('import hashmap', 'import hash\nimport hashmap\nimport keymap')
    body = re.sub(r'HashMap\.new\(&allocator, (\d+)\)', lambda m: f'KeyMap.new(&allocator, {m.group(1)}, {policy})', body)
    body = body.replace('HashMap<Key, U64>', 'KeyMap<Key, U64>')
    return head + 'fn main(' + body

def maps_program(r, upsert=True):
    """Random HashMap operations checked in the program against a Vec shadow (value + 1, 0 when
    absent): keys hashed so that runs wrap and collide, small and growing tables."""
    keys = r.randint(2, 24)
    hashing = r.choice(['(*self).id', '1000 - ((*self).id % 3)', '(*self).id % 2', '7', '(*self).id * 2654435761'])
    lines = ['import console', 'import hash', 'import hashmap', 'import discard', 'import vec', '',
             'type Key derive(Discard):', '    id: U64', '',
             'impl Hash for Key:', '    fn hash(self: &Key) -> U64:', f'        return {hashing}', '',
             'impl Eq for Key:', '    fn eq(self: &Key, other: &Key) -> Bool:', '        return (*self).id == (*other).id', '',
             'impl KeyFor<Key> for Key:', '    fn key_for(self: &Key, allocator: &Allocator) -> Result<Key, U64>:', '        return Ok(Key(id: (*self).id))', '',
             'fn main(console: Console<Write>, allocator: Allocator):',
             f'    let Ok(made): HashMap<Key, U64> = HashMap.new(&allocator, {r.choice([0, 1, 2, 3, 5, 8, 13, 64])}) else Err(e):',
             '        return',
             '    var map = move made',
             f'    let Ok(kept): Vec<U64> = Vec.filled(&allocator, {keys}, 0) else Err(e):',
             '        map.release()', '        return',
             '    var shadow = move kept',
             '    var bad: U64 = 0']
    for step in range(r.randint(10, 60)):
        k = r.randrange(keys); v = r.randrange(1000)
        op = r.choice(['put', 'put', 'remove', 'get', 'get_mut', 'contains'] + (['get_or_insert'] if upsert else []))
        if op == 'put':
            lines += [f'    match map.put(&allocator, Key(id: {k}), {v}):',
                      '        Ok:', f'            if shadow[{k}] != 0:', '                bad += 1', f'            shadow[{k}] = {v + 1}',
                      '        Err(back):', f'            if shadow[{k}] == 0:', '                bad += 1']
        elif op == 'remove':
            lines += [f'    match map.remove(&Key(id: {k})):',
                      '        Ok(entry):', f'            if shadow[{k}] != entry.value + 1:', '                bad += 1', f'            shadow[{k}] = 0',
                      '        Err(code):', f'            if shadow[{k}] != 0:', '                bad += 1']
        elif op == 'get':
            lines += [f'    match map.get(&Key(id: {k})):',
                      '        Ok(found):', f'            if shadow[{k}] != *found + 1:', '                bad += 1',
                      '        Err(code):', f'            if shadow[{k}] != 0:', '                bad += 1']
        elif op == 'get_mut':
            lines += [f'    match map.get_mut(&Key(id: {k})):',
                      '        Ok(found):', f'            if shadow[{k}] != *found + 1:', '                bad += 1',
                      f'            *found += {v}', f'            shadow[{k}] = shadow[{k}] + {v}',
                      '        Err(code):', f'            if shadow[{k}] != 0:', '                bad += 1']
        elif op == 'get_or_insert':
            lines += [f'    match map.get_or_insert(&allocator, &Key(id: {k}), {v}):',
                      '        Ok(found):', f'            if shadow[{k}] != 0 && shadow[{k}] != *found + 1:', '                bad += 1',
                      f'            if shadow[{k}] == 0 && *found != {v}:', '                bad += 1',
                      f'            *found += 1', f'            shadow[{k}] = *found + 1',
                      '        Err(missed):', '            missed.discard()', '            bad += 1']
        else:
            lines += [f'    if map.contains(&Key(id: {k})) != (shadow[{k}] != 0):', '        bad += 1']
    lines += ['    var expected: U64 = 0',
              f'    for k in 0..{keys}:', '        if shadow[k] != 0:', '            expected += 1',
              '    if map.len() != expected:', '        bad += 1',
              '    var cursor: U64 = 0', '    var drained: U64 = 0', '    var sum: U64 = 0', '    var going = true',
              '    while going:', '        match map.take_next(&mut cursor):',
              '            Ok(entry):', '                drained += 1', '                sum = (sum + entry.key.id * 1000 + entry.value) % 1000000007',  # in any order: a layout change is no difference
              '            Err(code):', '                going = false',
              '    console.print_int(bad)', '    console.print_int(drained)', '    console.print_int(sum)',
              '    map.release()', '    shadow.close()']
    return '\n'.join(lines) + '\n'

SHADOW = """
// The shadow does each change an element at a time, with push and index only.
fn shadow_append(s: &mut Vec<U64>, allocator: &Allocator, o: &Vec<U64>):
    for i in 0..o.len():
        match s.push_mut(allocator, o[i]):
            Ok:
                ()
            Err(back):
                ()

fn shadow_insert(s: &mut Vec<U64>, allocator: &Allocator, at: U64, x: U64):
    match s.push_mut(allocator, 0):
        Ok:
            ()
        Err(back):
            return
    var j = s.len() - 1
    while j > at:
        s[j] = s[j - 1]
        j -= 1
    s[at] = x

fn shadow_remove(s: &mut Vec<U64>, at: U64):
    for j in at..s.len() - 1:
        s[j] = s[j + 1]
    s.truncate_mut(s.len() - 1)

fn shadow_reverse(s: &mut Vec<U64>):
    var low: U64 = 0
    var high = s.len()
    while low + 1 < high:
        let t = s[low]
        s[low] = s[high - 1]
        s[high - 1] = t
        low += 1
        high -= 1
"""

def vecs_program(r):
    """Random Vec changes, the bulk copies among them, mirrored on a shadow Vec changed an
    element at a time; the two compared at the end."""
    lines = ['import console', 'import vec', SHADOW,
             'fn main(console: Console<Write>, allocator: Allocator):']
    made = []
    for name in ('v', 's', 'o'):
        lines += [f'    let Ok(made_{name}): Vec<U64> = Vec.new(&allocator, {r.choice([0, 1, 2, 4, 9])}) else Err(e):']
        lines += [f'        {m}.close()' for m in made] + ['        return', f'    var {name} = move made_{name}']
        made.append(name)
    lines += ['    var bad: U64 = 0']
    for step in range(r.randint(5, 40)):
        x = r.randrange(1000); at = r.randrange(50)
        op = r.choice(['push', 'push', 'grow', 'extend', 'push_all', 'insert', 'remove', 'reverse', 'truncate', 'swap', 'clear_other'])
        if op == 'push':
            lines += [f'    match v.push_mut(&allocator, {x}):', '        Ok:', f'            shadow_insert(&mut s, &allocator, s.len(), {x})', '        Err(back):', '            bad += 1']
        elif op == 'grow':
            lines += [f'    match o.push_mut(&allocator, {x}):', '        Ok:', '            ()', '        Err(back):', '            bad += 1']
        elif op == 'extend':
            lines += ['    if let Ok(_) = v.extend_copy_mut(&allocator, &o):', '        shadow_append(&mut s, &allocator, &o)', '    else:', '        bad += 1']
        elif op == 'push_all':
            lines += [f'    let room{step} = v.capacity() - v.len()', '    match v.push_all_copy_mut(&o):', '        Ok:',
                      f'            if o.len() > room{step}:', '                bad += 1', '            shadow_append(&mut s, &allocator, &o)',
                      '        Err(code):', f'            if o.len() <= room{step}:', '                bad += 1']
        elif op == 'insert':
            lines += [f'    let at{step} = {at} % (v.len() + 1)', f'    match v.insert_at(&allocator, at{step}, {x}):', '        Ok:',
                      f'            shadow_insert(&mut s, &allocator, at{step}, {x})', '        Err(back):', '            bad += 1']
        elif op == 'remove':
            lines += ['    if v.len() > 0:', f'        let at{step} = {at} % v.len()', f'        match v.remove_at(at{step}):', '            Ok(item):',
                      f'                if item != s[at{step}]:', '                    bad += 1', f'                shadow_remove(&mut s, at{step})',
                      '            Err(code):', '                bad += 1']
        elif op == 'reverse':
            lines += ['    v.reverse()', '    shadow_reverse(&mut s)']
        elif op == 'truncate':
            lines += [f'    let keep{step} = {at} % (v.len() + 2)', f'    v.truncate_mut(keep{step})', f'    if keep{step} < s.len():', f'        s.truncate_mut(keep{step})']
        elif op == 'swap':
            lines += ['    if v.len() > 0:', f'        let a{step} = {at} % v.len()', f'        let b{step} = {x} % v.len()',
                      f'        if let Ok(_) = v.swap(a{step}, b{step}):', f'            let t{step} = s[a{step}]', f'            s[a{step}] = s[b{step}]', f'            s[b{step}] = t{step}',
                      '        else:', '            bad += 1']
        else:
            lines += ['    o.clear_mut()']
    lines += ['    if v.len() != s.len():', '        bad += 1', '    var sum: U64 = 0',
              '    for i in 0..v.len():', '        if i < s.len() && v[i] != s[i]:', '            bad += 1', '        sum = (sum * 31 + v[i]) % 1000000007',
              '    console.print_int(bad)', '    console.print_int(v.len())', '    console.print_int(sum)',
              '    v.close()', '    s.close()', '    o.close()']
    return '\n'.join(lines) + '\n'

STRING_SHADOW = """
fn shadow_bytes(s: &mut Vec<U64>, allocator: &Allocator, from: &String, start: U64, end: U64):
    for i in start..end:
        match from.byte_at(i):
            Ok(b):
                match s.push_mut(allocator, U8.to_u64(b)):
                    Ok:
                        ()
                    Err(back):
                        ()
            Err(code):
                ()
"""

def strings_program(r):
    """Random ASCII String changes (`push_string` is the bulk copy, `push_part`, `push_text`,
    `push`, `cut`, `clear`) against a Vec of byte values changed a byte at a time."""
    lines = ['import console', 'import string', 'import vec', STRING_SHADOW,
             'fn main(console: Console<Write>, allocator: Allocator):',
             f'    let Ok(made_a): String = String.with_capacity(&allocator, {r.choice([0, 1, 3, 8, 40])}) else Err(e):', '        return', '    var a = move made_a',
             '    let Ok(made_b): String = String.new(&allocator) else Err(e):', '        a.close()', '        return', '    var b = move made_b',
             '    let Ok(made_s): Vec<U64> = Vec.new(&allocator, 0) else Err(e):', '        a.close()', '        b.close()', '        return', '    var s = move made_s',
             '    var bad: U64 = 0']
    pieces = ['', 'a', 'xyz', 'hello world', 'abcdefghijklmnopq', '0123456789abcdef!']
    for step in range(r.randint(5, 40)):
        op = r.choice(['text', 'text', 'push', 'grow', 'string', 'part', 'cut', 'clear_b'])
        if op == 'text':
            t = r.choice(pieces)
            lines += [f'    if let Ok(_) = a.push_text(&allocator, "{t}"):'] + [f'        match s.push_mut(&allocator, {ord(c)}):\n            Ok:\n                ()\n            Err(back):\n                ()' for c in t] + (['        ()'] if not t else []) + ['    else:', '        bad += 1']
        elif op == 'push':
            c = r.randrange(32, 127)
            lines += [f'    if let Ok(_) = a.push(&allocator, {c}):', f'        match s.push_mut(&allocator, {c}):', '            Ok:', '                ()', '            Err(back):', '                ()', '    else:', '        bad += 1']
        elif op == 'grow':
            lines += [f'    if let Err(_) = b.push_text(&allocator, "{r.choice(pieces[1:])}"):', '        bad += 1']
        elif op == 'string':
            lines += ['    if let Ok(_) = a.push_string(&allocator, &b):', '        shadow_bytes(&mut s, &allocator, &b, 0, b.byte_len())', '    else:', '        bad += 1']
        elif op == 'part':
            x, y = r.randrange(30), r.randrange(30)
            lines += [f'    let from{step} = {min(x, y)} % (b.byte_len() + 1)', f'    let to{step} = {max(x, y)} % (b.byte_len() + 1)',
                      f'    if from{step} <= to{step}:', f'        if let Ok(_) = a.push_part(&allocator, &b, from{step}, to{step}):',
                      f'            shadow_bytes(&mut s, &allocator, &b, from{step}, to{step})', '        else:', '            bad += 1']
        elif op == 'cut':
            lines += [f'    let keep{step} = {r.randrange(60)} % (a.byte_len() + 2)', f'    a.cut(keep{step})', f'    if keep{step} < s.len():', f'        s.truncate_mut(keep{step})']
        else:
            lines += ['    b.clear()']
    lines += ['    if a.byte_len() != s.len():', '        bad += 1', '    var sum: U64 = 0',
              '    for i in 0..s.len():', '        match a.byte_at(i):', '            Ok(byte):', '                if U8.to_u64(byte) != s[i]:', '                    bad += 1',
              '                sum = (sum * 31 + U8.to_u64(byte)) % 1000000007', '            Err(code):', '                bad += 1',
              '    console.print_int(bad)', '    console.print_int(a.byte_len())', '    console.print_int(sum)',
              '    a.close()', '    b.close()', '    s.close()']
    return '\n'.join(lines) + '\n'

def containers_program(r):
    """Random Deque and Heap operations against Vec shadows, and a sort checked for order and
    for keeping the same elements."""
    lines = ['import console', 'import vec', 'import deque', 'import heap', 'import sort', SHADOW,
             'fn least_at(s: &Vec<U64>) -> U64:', '    var at: U64 = 0', '    for i in 1..s.len():', '        if s[i] < s[at]:', '            at = i', '    return at', '',
             'fn main(console: Console<Write>, allocator: Allocator):',
             f'    let Ok(made_d): Deque<U64> = Deque.new(&allocator, {r.choice([0, 1, 2, 5])}) else Err(e):', '        return', '    var d = move made_d',
             '    let Ok(made_h): Heap<U64> = Heap.new(&allocator) else Err(e):', '        d.close()', '        return', '    var h = move made_h']
    made = ['d']
    for name in ('ds', 'hs', 'v', 'vs'):
        lines += [f'    let Ok(made_{name}): Vec<U64> = Vec.new(&allocator, 0) else Err(e):'] + [f'        {m}.close()' for m in made] + ['        h.discard()', '        return', f'    var {name} = move made_{name}']
        made.append(name)
    lines += ['    var bad: U64 = 0']
    for step in range(r.randint(10, 45)):
        x = r.randrange(r.choice([5, 100, 1 << 40])); at = r.randrange(40)
        op = r.choice(['back', 'front', 'pop_front', 'pop_back', 'get', 'hpush', 'hpush', 'hpop', 'first', 'vpush'])
        if op == 'back':
            lines += [f'    match d.push_back(&allocator, {x}):', '        Ok:', f'            shadow_insert(&mut ds, &allocator, ds.len(), {x})', '        Err(back):', '            bad += 1']
        elif op == 'front':
            lines += [f'    match d.push_front(&allocator, {x}):', '        Ok:', f'            shadow_insert(&mut ds, &allocator, 0, {x})', '        Err(back):', '            bad += 1']
        elif op == 'pop_front':
            lines += ['    match d.pop_front_mut():', '        Ok(item):', '            if ds.len() == 0 || item != ds[0]:', '                bad += 1',
                      '            if ds.len() > 0:', '                shadow_remove(&mut ds, 0)', '        Err(code):', '            if ds.len() != 0:', '                bad += 1']
        elif op == 'pop_back':
            lines += ['    match d.pop_back_mut():', '        Ok(item):', '            if ds.len() == 0 || item != ds[ds.len() - 1]:', '                bad += 1',
                      '            if ds.len() > 0:', '                ds.truncate_mut(ds.len() - 1)', '        Err(code):', '            if ds.len() != 0:', '                bad += 1']
        elif op == 'get':
            lines += ['    if ds.len() > 0:', f'        match d.get({at} % ds.len()):', '            Ok(item):', f'                if item != ds[{at} % ds.len()]:', '                    bad += 1',
                      '            Err(code):', '                bad += 1']
        elif op == 'hpush':
            lines += [f'    match h.push(&allocator, {x}):', '        Ok:', f'            shadow_insert(&mut hs, &allocator, hs.len(), {x})', '        Err(back):', '            bad += 1']
        elif op == 'hpop':
            lines += ['    match h.pop():', '        Ok(item):', '            if hs.len() == 0:', '                bad += 1', '            else:',
                      '                let at = least_at(&hs)', '                if item != hs[at]:', '                    bad += 1', '                shadow_remove(&mut hs, at)',
                      '        Err(code):', '            if hs.len() != 0:', '                bad += 1']
        elif op == 'first':
            lines += ['    match h.first():', '        Ok(item):', '            if hs.len() == 0 || *item != hs[least_at(&hs)]:', '                bad += 1',
                      '        Err(code):', '            if hs.len() != 0:', '                bad += 1']
        else:
            lines += [f'    match v.push_mut(&allocator, {x}):', '        Ok:', f'            shadow_insert(&mut vs, &allocator, vs.len(), {x})', '        Err(back):', '            bad += 1']
    lines += ['    if d.len() != ds.len() || h.len() != hs.len():', '        bad += 1',
              '    sort(&mut v)',
              '    if !is_sorted(&v) || v.len() != vs.len():', '        bad += 1',
              '    for i in 1..v.len():', '        if v[i] < v[i - 1]:', '            bad += 1',
              # The same elements: each shadow element is found as often in the sorted vector.
              '    for i in 0..vs.len():', '        var want: U64 = 0', '        var have: U64 = 0',
              '        for j in 0..vs.len():', '            if vs[j] == vs[i]:', '                want += 1', '            if v[j] == vs[i]:', '                have += 1',
              '        if want != have:', '            bad += 1',
              '    var sum: U64 = 0', '    for i in 0..v.len():', '        sum = (sum * 31 + v[i]) % 1000000007',
              '    console.print_int(bad)', '    console.print_int(sum)',
              '    d.close()', '    h.discard()', '    ds.close()', '    hs.close()', '    v.close()', '    vs.close()']
    return '\n'.join(lines) + '\n'

def generators_program(r):
    """A random `gen fn` read twice: by `for v in g(...)`, which may run it in place, and through a
    variable, which steps it; the two readings must yield the same values in the same order."""
    loops = [0]
    def body(depth, names):
        out = []
        for _ in range(r.randint(1, 3)):
            choice = r.random()
            if choice < 0.4:
                out.append(f'yield ({expr(r, names, 1)}) % 1000003')
            elif choice < 0.7 and depth < 2:
                v = f'i{loops[0]}'; loops[0] += 1  # a `gen fn` names each local once
                out.append(f'for {v} in 0..{r.randint(0, 5)}:')
                out += ['    ' + l for l in body(depth + 1, names + [v])]
            elif choice < 0.9 and depth < 2:
                out.append(f'if ({expr(r, names, 1)}) % 3 == 0:')
                out += ['    ' + l for l in body(depth + 1, names)]
                if r.random() < 0.5:
                    out.append('else:')
                    out += ['    ' + l for l in body(depth + 1, names)]
            else:
                out.append(f'yield {r.randrange(100)}')
        return out
    gen = ['gen fn g(a: U64, b: U64) -> U64:'] + ['    ' + l for l in body(0, ['a', 'b'])]
    calls = [(r.randrange(50), r.randrange(50)) for _ in range(r.randint(1, 4))]
    lines = ['import console', '', *gen, '', 'fn main(console: Console<Write>):', '    var bad: U64 = 0', '    var total: U64 = 0']
    for n, (x, y) in enumerate(calls):
        lines += [f'    var in_place{n}: U64 = 0', f'    var count{n}: U64 = 0', f'    for v in g({x}, {y}):', f'        in_place{n} = (in_place{n} * 31 + v) % 1000000007', f'        count{n} += 1',
                  f'    var stepped{n}: U64 = 0', f'    var steps{n}: U64 = 0', f'    var s{n} = g({x}, {y})', f'    for v in s{n}:', f'        stepped{n} = (stepped{n} * 31 + v) % 1000000007', f'        steps{n} += 1',
                  f'    if in_place{n} != stepped{n} || count{n} != steps{n}:', '        bad += 1', f'    total = (total * 1000003 + in_place{n} + count{n}) % 1000000007']
    lines += ['    console.print_int(bad)', '    console.print_int(total)']
    return '\n'.join(lines) + '\n'

def crossgen_program(r, k):
    """Another file's `gen fn` whose body calls its file's public and private helpers and a function
    through its own import (`util.pick`), read from a program that declares helpers of the same
    names and imports a different util.wy: in place, stepped, and by the generator's own file must
    all agree (reviews 8 and 9 of 2026-11-19: names and qualifiers were read in the caller's file)."""
    tag = f'{seed}_{k}'
    module = f'gm{tag}'
    for side, add in (('a', 1), ('b', 1000)):
        os.makedirs(os.path.join(out, f'u{tag}{side}'), exist_ok=True)
        open(os.path.join(out, f'u{tag}{side}', 'util.wy'), 'w').write(f'pub fn pick(x: U64) -> U64:\n    return (x + {add}) % 1000003\n')
    loops = [0]
    def body(depth, names):
        out_lines = []
        for _ in range(r.randint(1, 3)):
            choice = r.random()
            if choice < 0.45:
                call = r.choice(['helper', 'secret', 'util.pick', ''])
                value = f'({expr(r, names, 1)}) % 1000003'
                out_lines.append(f'yield {call}({value})' if call else f'yield {value}')
            elif choice < 0.75 and depth < 2:
                v = f'i{loops[0]}'; loops[0] += 1
                out_lines.append(f'for {v} in 0..{r.randint(0, 4)}:')
                out_lines += ['    ' + l for l in body(depth + 1, names + [v])]
            elif depth < 2:
                out_lines.append(f'if ({expr(r, names, 1)}) % 3 == 0:')
                out_lines += ['    ' + l for l in body(depth + 1, names)]
            else:
                out_lines.append(f'yield {r.randrange(100)}')
        return out_lines
    gen = ['pub gen fn walk(a: U64, b: U64) -> U64:'] + ['    ' + l for l in body(0, ['a', 'b'])]
    mod = [f'import "u{tag}a/util.wy"', '',
           'pub fn helper(x: U64) -> U64:', '    return (x * 3 + 1) % 1000003', '',
           'fn secret(x: U64) -> U64:', '    return (x + 7) % 1000003', '', *gen, '',
           'pub fn own(a: U64, b: U64) -> U64:', '    var sum: U64 = 0', '    var count: U64 = 0',
           '    for v in walk(a, b):', '        sum = (sum * 31 + v) % 1000000007', '        count += 1', '    return (sum * 1000 + count) % 1000000007']
    open(os.path.join(out, f'{module}.wy'), 'w').write('\n'.join(mod) + '\n')
    calls = [(r.randrange(40), r.randrange(40)) for _ in range(r.randint(1, 3))]
    lines = ['import console', f'import "{module}.wy"', f'import "u{tag}b/util.wy"', '',
             'fn helper(x: U64) -> U64:', '    return (x + 500) % 1000003', '',
             'fn secret(x: U64) -> U64:', '    return (x + 900) % 1000003', '',
             'fn main(console: Console<Write>):', '    var bad: U64 = 0', '    var total: U64 = 0',
             '    total = (total + helper(1) + secret(1) + util.pick(1)) % 1000000007']
    for n, (x, y) in enumerate(calls):
        lines += [f'    var a{n}: U64 = 0', f'    var c{n}: U64 = 0', f'    for v in walk({x}, {y}):', f'        a{n} = (a{n} * 31 + v) % 1000000007', f'        c{n} += 1',
                  f'    var b{n}: U64 = 0', f'    var d{n}: U64 = 0', f'    var s{n} = walk({x}, {y})', f'    for v in s{n}:', f'        b{n} = (b{n} * 31 + v) % 1000000007', f'        d{n} += 1',
                  f'    let in_place{n} = (a{n} * 1000 + c{n}) % 1000000007', f'    let stepped{n} = (b{n} * 1000 + d{n}) % 1000000007',
                  f'    if in_place{n} != stepped{n} || in_place{n} != {module}.own({x}, {y}):', '        bad += 1',
                  f'    total = (total * 1000003 + in_place{n}) % 1000000007']
    lines += ['    console.print_int(bad)', '    console.print_int(total)']
    return '\n'.join(lines) + '\n'

def program(r, k):
    if mode == 'strings': return strings_program(r)
    if mode == 'maps': return maps_program(r)
    if mode == 'crossgen': return crossgen_program(r, k)
    if mode == 'generators': return generators_program(r)
    if mode == 'containers': return containers_program(r)
    if mode == 'keymaps': return keymaps_program(r)
    if mode == 'derived': return derived_program(r)
    if mode == 'vecs': return vecs_program(r)
    global signed
    signed = mode == 'i64' or (mode == 'mixed' and k % 2 == 1)
    T = 'I64' if signed else 'U64'
    lines=['import console','import int','']
    helpers=[]
    for h in range(r.randint(1,3)):
        name=f'h{h}'; helpers.append(name)
        body=[f'fn {name}(a: {T}, b: {T}) -> {T}:']
        if r.random()<0.5:
            body+= [f'    if a > {r.choice([5,100,1000])}:', f'        return {expr(r,["a","b"],2)}']
        if r.random()<0.4:
            body+= [f'    let t: {T} = {expr(r,["a","b"],2)}']
            body+= [f'    return {expr(r,["a","b","t"],2)}']
        else:
            body+= [f'    return {expr(r,["a","b"],2)}']
        lines+=body+['']
    lines+=['fn main(console: Console<Write>):']
    names=[]
    for v in range(r.randint(2,5)):
        nm=f'v{v}'
        e=expr(r,names,2)
        if helpers and r.random()<0.5: e=f'{r.choice(helpers)}({expr(r,names,1)}, {expr(r,names,1)})'
        lines.append(f'    var {nm}: {T} = {e}'); names.append(nm)
    lines.append(f'    for i in 0..{r.randint(1,20)}:')
    tgt=r.choice(names)
    counter = 'I64.from_bits(i)' if signed else 'i'
    lines.append(f'        {tgt} = ({expr(r,names+[counter],2)}) % 1000003')
    if mode == 'format':
        # Several interpolations in one program (their records are named apart, 2026-11-19).
        for p in range(r.randint(2, 4)):
            picked = r.sample(names, min(len(names), r.randint(1, 3)))
            lines.append('    console.print("' + ' '.join(f'{nm}=${{{nm}}}' for nm in picked) + f' #{p}")')
    else:
        for nm in names: lines.append(f'    console.print_{"i64" if signed else "int"}({nm})')
    if mode == 'vec':
        lines[0:0] = ['import vec']
        lines = [l.replace('fn main(console: Console<Write>):', 'fn main(console: Console<Write>, allocator: Allocator):') for l in lines]
        count = r.randint(0, 40)
        lines += ['    let Ok(made): Vec<U64> = Vec.new(&allocator, %d) else Err(e):' % r.randint(0, 8),
                  '        return',
                  '    var items = move made',
                  '    for k in 0..%d:' % count,
                  '        match items.push_mut(&allocator, (%s) %% 1000003):' % expr(r, names + ['k'], 2),
                  '            Ok:',
                  '                ()',
                  '            Err(back):',
                  '                console.line("full")',
                  '    var total: U64 = 0',
                  '    for k in 0..items.len():',
                  '        match items.get(k):',
                  '            Ok(item):',
                  '                total = (total * 31 + item) % 1000000007',
                  '            Err(e):',
                  '                ()',
                  '        match items.at_mut(k):',
                  '            Ok(slot):',
                  '                *slot = (*slot + %s) %% 1000003' % expr(r, names, 1),
                  '            Err(e):',
                  '                ()',
                  '    console.print_int(total)',
                  '    console.print_int(items.len())',
                  '    items.close()']
    if mode == 'records':
        head = ['type Pair:', '    left: U64', '    right: U64', '',
                'type Box:', '    pair: Pair', '    count: U64', '    small: U8', '',
                'type Shape:', '    Dot(U64)', '    Two(Pair)', '    Nothing', '',
                'fn weigh(b: Box) -> U64:',
                '    return (b.pair.left * 3 + b.pair.right + b.count + U8.to_u64(b.small)) % 1000003', '',
                'fn pick(n: U64, p: Pair) -> Shape:',
                '    if n % 3 == 0:', '        return Dot(n)',
                '    if n % 3 == 1:', '        return Two(p)',
                '    return Nothing', '']
        at = lines.index('fn main(console: Console<Write>):')
        lines[at:at] = head
        lines += ['    var total: U64 = 0']
        for k in range(r.randint(1, 6)):
            a1, a2, a3 = expr(r, names, 1), expr(r, names, 1), expr(r, names, 1)
            lines += ['    let p%d = Pair(%s %% 100000, %s %% 100000)' % (k, a1, a2),
                      '    let b%d = Box(p%d, %s %% 1000, U8.truncate(%s %% 256))' % (k, k, a3, expr(r, names, 1)),
                      '    total = (total + weigh(b%d)) %% 1000003' % k,
                      '    match pick(%s, p%d):' % (expr(r, names, 1), k),
                      '        Dot(n):', '            total = (total + n % 1000) % 1000003',
                      '        Two(q):', '            total = (total + q.left * 7 + q.right) % 1000003',
                      '        Nothing:', '            total = (total + 1) % 1000003']
        lines += ['    console.print_int(total)']
    if mode == 'text':
        lines[0:0] = ['import string', 'import text']
        lines = [l.replace('fn main(console: Console<Write>):', 'fn main(console: Console<Write>, allocator: Allocator):') for l in lines]
        pieces = ['ab', 'é', 'héllo ', '€', '𝄞', 'xyz', '', 'aa']
        lines += ['    let Ok(made): String = String.new(&allocator) else Err(e):', '        return', '    var s = move made']
        for k in range(r.randint(1, 12)):
            if r.random() < 0.5:
                lines.append('    if let Err(_) = s.push_text(&allocator, "%s"):' % r.choice(pieces))
            else:
                lines.append('    if let Err(_) = s.push(&allocator, (%s) %% 300 + %d):' % (expr(r, names, 1), r.choice([32, 97, 200, 8000])))
            lines.append('        console.line("no room")')
        needle, repl = r.choice(pieces[:-2] + ['a']), r.choice(pieces)
        lines += ['    console.print_int(s.byte_len())',
                  '    console.print_int(s.scalar_count())',
                  '    var check: U64 = 0',
                  '    for c in &s:',
                  '        check = (check * 31 + c) % 1000000007',
                  '    console.print_int(check)',
                  '    match s.replace_text(&allocator, "%s", "%s"):' % (needle, repl),
                  '        Ok(out):',
                  '            console.write_string(&out)',
                  '            console.line("")',
                  '            console.print_int(out.byte_len())',
                  '            out.close()',
                  '        Err(e):',
                  '            console.line("no room")',
                  '    s.close()']
    return '\n'.join(lines)+'\n'
def run(compiler, src, tag):
    w=src[:-3]+tag+'.wasm'
    c=subprocess.run([compiler,'wasm-checked',src,'-o',w],env=env,capture_output=True,text=True)
    if c.returncode!=0:
        # Once more before a refusal is believed: the proof helper's deadline fires on a loaded machine.
        c=subprocess.run([compiler,'wasm-checked',src,'-o',w],env=env,capture_output=True,text=True)
    if c.returncode!=0:
        first=[l for l in (c.stdout+c.stderr).splitlines() if 'W0' in l]
        # The message, without the byte offsets (the standard library before it may differ in size).
        message = first[0].split(': ',1)[-1] if first else 'unknown'
        return 'refused: '+re.sub(r'W0\d+(?: \d+\.\.\d+| \([^)]*\))?: ', '', message)
    p=subprocess.run([host,w,'--output','--fuel=50000000']+(['--allocator'] if mode in ('vec', 'text', 'maps', 'vecs', 'strings', 'keymaps', 'derived', 'containers') else []),capture_output=True,text=True)
    lines=(p.stdout+p.stderr).splitlines()
    # The engine's own lines are prefixed `witchy: ` (5f315910). A backtrace's frames carry code
    # offsets, which differ between two compilers: those lines are not compared.
    said=lambda l: l[len('witchy: '):].strip() if l.startswith('witchy: ') else l.strip()
    keep=[l for l in lines if not (re.match(r'\d+:', said(l)) or said(l).startswith(('error while','trap:')))]
    return '\n'.join(keep)
def one(k):
    r=random.Random(seed*100000+k)
    src=os.path.join(out,f'p{seed}_{k}.wy'); open(src,'w').write(program(r, k))
    a=run(old,src,'.old'); b=run(new,src,'.new')
    return (k, a==b, a, b, src)
# These programs check themselves: the first line printed is the number of mismatches they found,
# and a program that does not run to that line is a failure too.
SELF_CHECKING = ('maps', 'keymaps', 'vecs', 'strings', 'derived', 'containers', 'generators', 'crossgen')
diffs=0; kinds={}; failed=0; refused=0
with cf.ThreadPoolExecutor(int(os.environ.get('DIFF_COMPILERS_JOBS', '8'))) as ex:
    for k,same,a,b,src in ex.map(one, range(n)):
        kind=('refused' if a.startswith('refused') else 'trap' if 'stopped' in a else 'ran')
        kinds[kind]=kinds.get(kind,0)+1
        if a.startswith('refused') and b.startswith('refused'):
            refused+=1
            if refused<=3: print('REFUSED',src,'\n both:',b[:300])
        if not same:
            diffs+=1
            if diffs<=5: print('DIFF',src,'\n old:',a[:300],'\n new:',b[:300])
        if mode in SELF_CHECKING and (b.splitlines() or [''])[0] != '0':
            failed+=1
            if failed<=5: print('SELF-CHECK',src,'\n new:',b[:300])
print('programs',n,kinds,'differences',diffs,*(['self-check failures',failed] if mode in SELF_CHECKING else []),'both refused',refused)
# Programs that neither compiler builds compare equal and test nothing: most of them refused is a
# stale generator, not agreement.
if refused*2>n: print('FAIL: both compilers refused more than half of the programs; the generator is stale')
sys.exit(1 if diffs or failed or refused*2>n else 0)
