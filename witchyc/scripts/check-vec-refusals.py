#!/usr/bin/env python3
"""Refusal controls for the proofs in library/wasm-vec.wy.

Each case edits one place in a copy of the library so that a proved function no
longer keeps its contract or the Vec invariant, and builds a client of the copy.
The build must fail with a proof failure that names the function.

    scripts/check-vec-refusals.py --compiler PATH --binding READY-BINDING [--work DIR] [FIRST [LAST]]

About four minutes for all cases. The library in the repository is not changed.
"""
import argparse, os, subprocess, sys, tempfile

CLIENT = '''import "wasm-vec.wy"

fn main(console: Console<Write>, allocator: Allocator):
    let made: Result<Vec<U64>, U64> = Vec.new(&allocator, 4)
    match move made:
        Ok(empty):
            empty.close()
        Err(code):
            console.print_int(code)
'''

cases=[
 ('memory_take_prefix', '    requires memory.initialized_range(buffer, 0, count)\n', ''),
 ('memory_take_prefix', '    requires count <= memory.capacity(buffer)\n    requires memory.initialized_range(buffer, 0, count)', '    requires memory.initialized_range(buffer, 0, count)'),
 ('Vec.close', '        memory_take_prefix(&mut buffer, length)\n', '        memory_take_prefix(&mut buffer, length + 1)\n'),
 ('Vec.close', '        memory_take_prefix(&mut buffer, length)\n', ''),
 ('memory_move_prefix', '    requires memory.initialized_range(source, 0, count)\n', ''),
 ('memory_move_prefix', '    requires memory.empty_range(target, 0, count)\n', ''),
 ('memory_move_prefix', '    requires count <= memory.capacity(source)\n', ''),
 ('Vec.reserve', 'memory_move_prefix(&mut old_buffer, &mut new_buffer, logical_length)', 'memory_move_prefix(&mut old_buffer, &mut new_buffer, logical_length + 1)'),
 ('Vec.reserve', '                memory_move_prefix(&mut old_buffer, &mut new_buffer, logical_length)\n', ''),
 ('memory_release_empty', '    requires memory.empty_all(input)\n', ''),
 ('Vec.push_spare', '            vector.length = index + 1\n            return Ok(move vector)', '            return Ok(move vector)'),
 ('Vec.push_spare', '            vector.length = index + 1\n            return Ok(move vector)', '            vector.length = index + 2\n            return Ok(move vector)'),
 ('Vec.push_spare_mut', '            (*vector).length = index + 1\n', '            (*vector).length = index\n'),
 ('Vec.take_last_mut', '        (*vector).length = index\n        return Ok(move item)', '        return Ok(move item)'),
 ('Vec.take_last_mut', '        if length == 0:\n            return Err(2)\n', ''),
 ('Vec.take_last_mut', '        let item = memory.take_unchecked(&mut (*vector).data, index)', '        let item = memory.take_unchecked(&mut (*vector).data, length)'),
 ('Vec.take_last', '                out.vector.length + 1 == old(input.length)', '                out.vector.length + 2 == old(input.length)'),
 ('Vec.take_last', 'memory.cell(old(input.data), out.vector.length, out.item)', 'memory.cell(old(input.data), out.vector.length + 1, out.item)'),
 ('Vec.new', 'return Ok(Vec(move buffer, 0, requested))', 'return Ok(Vec(move buffer, 1, requested))'),
 ('Vec.reserve', 'return Ok(Vec(move new_buffer, logical_length, requested))', 'return Ok(Vec(move new_buffer, logical_length, requested + 1))'),
 ('Vec.get', '        if index < (*value).length:\n            return Ok(memory.get_unchecked', '        if index <= (*value).length:\n            return Ok(memory.get_unchecked'),
 ('Vec.take_last', 'memory.unchanged_except(out.vector.data, old(input.data), out.vector.length)\n            Err(back):\n                memory.same_allocation(back.data, old(input.data))\n                back.length == old(input.length)', 'memory.unchanged_except(out.vector.data, old(input.data), out.vector.length)\n            Err(back):\n                memory.same_allocation(back.data, old(input.data))\n                back.length == 1'),
 ('Vec.take_last', '            Ok(item):\n                return Ok(VecTaken(move vector, move item))\n            Err(code):\n                return Err(move vector)', '            Ok(item):\n                vector.length = vector.length + 1\n                return Ok(VecTaken(move vector, move item))\n            Err(code):\n                return Err(move vector)'),
 ('Vec.push_spare_mut', '                vector.length == old(vector.length) + 1\n                memory.cell(vector.data, old(vector.length), item)', '                vector.length == old(vector.length) + 2\n                memory.cell(vector.data, old(vector.length), item)'),
 ('Vec.push_spare_mut', 'memory.cell(vector.data, old(vector.length), item)', 'memory.cell(vector.data, old(vector.length) + 1, item)'),
 ('Vec.push_spare_mut', '                old(vector.length) == old(vector.capacity)\n', '                old(vector.length) == 0\n'),
 ('Vec.take_last_mut', '                code == 2\n                old(vector.length) == 0', '                code == 3\n                old(vector.length) == 0'),
 ('Vec.take_last_mut', 'memory.cell(old(vector.data), vector.length, item)', 'memory.cell(old(vector.data), vector.length + 1, item)'),
 ('Vec.take_last', 'memory.unchanged_except(vector.data, old(vector.data), vector.length)', 'memory.unchanged_except(vector.data, old(vector.data), 0)'),
 ('Vec.at', '        if index < (*value).length:\n            return Ok(memory.ref_unchecked', '        if index <= (*value).length:\n            return Ok(memory.ref_unchecked'),
 ('Vec.at', '                index < value.length\n                memory.cell(value.data, index, item)', '                index < value.length\n                memory.cell(value.data, index + 1, item)'),
 ('Vec.at', '        if index < (*value).length:\n            return Ok(memory.ref_unchecked(&(*value).data, index))\n        return Err(1)', '        return Ok(memory.ref_unchecked(&(*value).data, index))'),
 ('Vec.replace_mut', '        if index < (*vector).length:\n            let before', '        if index <= (*vector).length:\n            let before'),
 ('Vec.replace_mut', 'memory_init_unchecked(&mut (*vector).data, index, move item)\n            return Ok(move before)', 'memory_init_unchecked(&mut (*vector).data, index + 1, move item)\n            return Ok(move before)'),
 ('Vec.replace_mut', '                memory.cell(old(vector.data), index, taken)\n                memory.cell(vector.data, index, item)', '                memory.cell(old(vector.data), index, taken)\n                memory.cell(vector.data, index, taken)'),
 ('Vec.replace_mut', '        ensures vector.length == old(vector.length)\n        ensures match result:\n            Ok(taken):', '        ensures vector.length == old(vector.length) + 1\n        ensures match result:\n            Ok(taken):'),
 ('Vec.replace_mut', '                index >= vector.length\n                memory.unchanged(vector.data, old(vector.data))\n        proof:\n            auto\n        if index < (*vector).length:\n            let before', '                index > vector.length\n                memory.unchanged(vector.data, old(vector.data))\n        proof:\n            auto\n        if index < (*vector).length:\n            let before'),
 ('Vec.at_mut', '        if index < (*value).length:\n            return Ok(memory.mut_unchecked', '        if index <= (*value).length:\n            return Ok(memory.mut_unchecked'),
 ('Vec.at_mut', '        if index < (*value).length:\n            return Ok(memory.mut_unchecked(&mut (*value).data, index))\n        return Err(1)', '        return Ok(memory.mut_unchecked(&mut (*value).data, index))'),
 ('Vec.at_mut', '            Ok(item):\n                index < value.length\n            Err(code):\n                code == 1', '            Ok(item):\n                index < value.length\n                memory.cell(old(value.data), index, item)\n            Err(code):\n                code == 1'),
 ('Vec.at_mut', '            Ok(item):\n                index < value.length\n            Err(code):\n                code == 1', '            Ok(item):\n                index < value.length\n                memory.unchanged(value.data, old(value.data))\n            Err(code):\n                code == 1'),
 ('Vec.at_mut', '        ensures value.length == old(value.length)\n        ensures value.capacity == old(value.capacity)\n        ensures match result:\n            Ok(item):\n                index < value.length\n            Err(code):', '        ensures value.length == old(value.length) + 1\n        ensures value.capacity == old(value.capacity)\n        ensures match result:\n            Ok(item):\n                index < value.length\n            Err(code):'),
 ('Vec.at_mut', 'return Ok(memory.mut_unchecked(&mut (*value).data, index))', 'return Ok(memory.mut_unchecked(&mut (*value).data, index + 1))'),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--compiler', required=True)
    parser.add_argument('--binding', required=True)
    parser.add_argument('--work')
    parser.add_argument('first', nargs='?', type=int, default=0)
    parser.add_argument('last', nargs='?', type=int)
    args = parser.parse_args()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    lib = open(os.path.join(root, 'library', 'wasm-vec.wy')).read()
    work = args.work or tempfile.mkdtemp(prefix='witchy-vec-refusals-')
    os.makedirs(work, exist_ok=True)
    open(os.path.join(work, 'v.wy'), 'w').write(CLIENT)
    env = dict(os.environ, WITCHY_WASM_READY_BINDING=args.binding)
    env.pop('WITCHY_PROOF_CACHE', None)
    bad = 0
    for fn, old, new in cases[args.first:args.last]:
        if lib.count(old) != 1:
            print('STALE CASE', fn, '|', old[:60].replace('\n', ' '))
            bad += 1
            continue
        open(os.path.join(work, 'wasm-vec.wy'), 'w').write(lib.replace(old, new))
        r = subprocess.run([args.compiler, 'wasm-measure', os.path.join(work, 'v.wy'), '-o', os.path.join(work, 'v.wasm')],
                           capture_output=True, text=True, env=env)
        lines = [l for l in r.stderr.split('\n') if 'proof failed' in l or 'not proved' in l or l.startswith('W04') or ': W04' in l]
        ok = r.returncode != 0 and any(f'`{fn}`' in l for l in lines)
        bad += not ok
        print('refused' if ok else 'NOT REFUSED', fn, '|', ' / '.join(l.strip()[-150:] for l in lines[:2]), flush=True)
    print('cases', len(cases[args.first:args.last]), 'bad', bad)
    sys.exit(1 if bad else 0)

if __name__ == '__main__':
    main()
