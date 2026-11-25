#!/usr/bin/env python3
"""Focused actual Vec constructor proofs/refusals and Wasmtime execution; reserve host first."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import time


def sha(data):
    return hashlib.sha256(data).hexdigest()

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--compiler', type=Path, required=True)
p.add_argument('--binding', type=Path, required=True)
p.add_argument('--runtime', type=Path, required=True)
p.add_argument('--out', type=Path, required=True)
p.add_argument('--case', choices=['positive', 'negative', 'renamed', 'pop-outcomes', 'pop-negative', 'get-outcomes', 'get-negative'], required=True)
p.add_argument('--reference', type=Path)
a = p.parse_args()
repo = Path(__file__).resolve().parents[1]
assert shutil.disk_usage(a.out.parent).free >= 10 * 2**30, '10 GiB launch floor'
a.out.mkdir()
library = (repo / 'library/wasm-vec.wy').read_text()
client = (repo / ('tests/fixtures/vec-constructor/pop-outcomes.wy' if a.case in ['pop-outcomes', 'pop-negative'] else 'tests/fixtures/vec-constructor/roundtrip.wy')).read_text()
if a.case in ['get-outcomes', 'get-negative']:
    client += '\n    get_cases(&console, &allocator)\n'
    client = (repo / 'tests/fixtures/vec-constructor/get-outcomes.wy').read_text() + client
source = library + client
program, output = a.out / 'program.wy', a.out / 'program.wasm'
env = {k: v for k, v in os.environ.items()
       if not k.startswith(('WITCHY_', 'LEAN_', 'LAKE_', 'NODE_', 'DYLD_', 'LD_'))}
env.update(WITCHY_WASM_READY_BINDING=str(a.binding), WITCHY_WASMTIME_HOST=str(a.runtime))
report = dict(status='running', case=a.case, commands=[], controls=[],
              source_sha256=sha(source.encode()), compiler_sha256=sha(a.compiler.read_bytes()))

def save():
    pending = a.out / 'results.pending'
    pending.write_text(json.dumps(report, indent=2) + '\n')
    pending.replace(a.out / 'results.json')


def invoke(args, name):
    assert shutil.disk_usage(a.out).free >= 10 * 2**30, '10 GiB launch floor'
    census = subprocess.check_output(['/bin/ps', '-axo', 'pid=,comm='], text=True).splitlines()
    heavy = [row for row in census if Path(row.split(None, 1)[1]).name in
             {'rustc', 'cargo', 'mbx', 'lean', 'leanc', 'proof-session-helper',
              'witchyc', 'compiler.bin', 'witchy-wasmtime-host'}]
    assert not heavy, 'host is not quiet: ' + repr(heavy)
    (a.out / (name + '.census')).write_text('\n'.join(census) + '\n')
    stdout, stderr = a.out / (name + '.stdout'), a.out / (name + '.stderr')
    started = time.monotonic()
    child = None
    with stdout.open('wb') as out, stderr.open('wb') as err:
        try:
            child = subprocess.Popen(list(map(str, args)), env=env,
                                     stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                     start_new_session=True)
            while child.poll() is None:
                assert shutil.disk_usage(a.out).free >= 4 * 2**30, '4 GiB live floor'
                assert time.monotonic() - started < 60, 'focused command deadline'
                assert stdout.stat().st_size < 2**20 and stderr.stat().st_size < 2**20
                time.sleep(.05)
            child.wait(timeout=5)
        finally:
            if child is not None:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGTERM)
                    try:
                        child.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait(timeout=5)
                # Production helpers inherit this newly owned process group.
                try:
                    os.killpg(child.pid, 0)
                except ProcessLookupError:
                    pass
                else:
                    raise AssertionError('owned group remains; keep host reservation')
    report['commands'].append(dict(name=name, argv=list(map(str, args)),
        exit=child.returncode, seconds=time.monotonic() - started,
        pid=child.pid, owned_group_absent=True))
    save()
    return child.returncode, stdout.read_bytes(), stderr.read_text()


def compile_text(text, name):
    program.write_text(text)
    (a.out / (name + '.wy')).write_text(text)
    return invoke([a.compiler, 'wasm-checked', program, '-o', output], name)


def execute(name):
    code, data, errors = invoke([a.runtime, output,
                                '--output', '--allocator'], name)
    expected = [73, 18446744073709551615, 1, 73, 4294967296, 2, 0, 80]
    if a.case in ['pop-outcomes', 'pop-negative']:
        expected = [0, 0, 0, 404, 37, 1, 2, 2, 0, 11, 99,
                    1, 99, 1, 73, 18446744073709551615, 1, 1, 73,
                    2, 2, 2, 73, 0, 2, 2, 404]
    if a.case in ['get-outcomes', 'get-negative']:
        expected += [1, 0, 2, 2, 404, 73, 1, 2, 2, 73, 37,
                     73, 1, 2, 2, 73, 1, 1, 2, 2, 73, 1, 1, 2, 2, 73,
                     99, 18446744073709551615, 0, 2, 99,
                     1, 18446744073709551615, 0, 2, 99,
                     1, 18446744073709551615, 0, 2, 99,
                     1, 18446744073709551615, 0, 2, 99]
    assert code == 0 and not errors and data.decode() == ''.join(f'{v}\n' for v in expected), (code, data, errors)
    report['runtime_output_lines'] = len(expected)


def constructor(change):
    start = library.index('fn vec_new<T>')
    end = library.index('fn vec_len<T>')
    original = library[start:end]
    changed = change(original)
    assert original != changed
    return library[:start] + changed + library[end:] + 'fn main():\n    return\n'


try:
    save()
    if a.case in ['positive', 'renamed', 'pop-outcomes', 'get-outcomes']:
        if a.case == 'renamed':
            source = source.replace('Vec', 'Parcel').replace('vec_', 'parcel_')
            report['source_sha256'] = sha(source.encode())
        code, data, errors = compile_text(source, 'positive')
        assert code == 0 and not errors, (code, errors)
        receipt = json.loads(data)
        assert receipt['source_sha256'] == sha(program.read_bytes())
        assert receipt['wasm_sha256'] == sha(output.read_bytes())
        assert receipt['proof_admission'] is False
        obligations = receipt['obligations']
        assert all(g['outcome'] == 'fresh_kernel_checked' for g in obligations)
        names = (['parcel_new', 'independent_storage', 'parcel_take_last'] if a.case == 'renamed' else
                 ['vec_new', 'vec_take_last'] if a.case == 'pop-outcomes' else
                 ['vec_new', 'independent_storage', 'vec_take_last'])
        for name in names:
            assert any(g['function'] == name and g['goal_kind'] == 'function_body' and
                       g.get('source_model') == 'witchy.checked-storage-record.v5' for g in obligations)
        if a.case != 'pop-outcomes':
            assert any(g.get('callee') == names[0] and g['function'] == 'roundtrip' for g in obligations)
        if a.case == 'get-outcomes':
            assert any(g['function'] == 'vec_get' and g['goal_kind'] == 'function_body' and
                       g.get('source_model') == 'witchy.proof-required-memory.v4' for g in obligations)
        assert len(receipt['selected_generation_kernel_checks']['audited_dependencies']) == len(obligations)
        report['obligations'] = obligations
        report['selected_generation_kernel_checks'] = receipt['selected_generation_kernel_checks']
        report['wasm_sha256'] = receipt['wasm_sha256']
        execute('wasmtime')
    else:
        assert a.reference and a.reference.is_file()
        reference = a.reference.read_bytes()
        output.write_bytes(reference)
        definitions = library + 'fn main():\n    return\n'
        tests = [
            ('changed-capacity', constructor(lambda s: s.replace('output.capacity == requested', 'output.capacity == 0')), 'W0402'),
            ('nonempty-prefix', constructor(lambda s: s.replace('memory.prefix(output.data, 0, requested)', 'memory.prefix(output.data, 1, requested)')), 'W0402'),
            ('fake-failure-code', constructor(lambda s: s.replace('        Err(code):\n            true', '        Err(code):\n            code == 0')), 'W0402'),
            ('wrong-body-capacity', constructor(lambda s: s.replace('Vec(move buffer, 0, requested)', 'Vec(move buffer, 0, 0)')), 'W0402'),
            ('leaked-fresh-owner', constructor(lambda s: s.replace('return Ok(Vec(move buffer, 0, requested))', 'return Err(0)')), 'W0401'),
            ('duplicate-owner', definitions + 'fn duplicate<T>(input: Vec<T>):\n    vec_close(move input)\n    vec_close(move input)\n', 'W0401'),
            ('use-after-move', definitions + 'fn after_move<T>(input: Vec<T>) -> U64:\n    vec_close(move input)\n    return input.length\n', 'W0401'),
            ('invalid-permission', definitions + 'fn shared_write<T>(buffer: &Buffer<T>, item: T):\n    memory.init_unchecked(buffer, 0, item)\n', 'W0401'),
        ]
        if a.case == 'pop-negative':
            start, end = library.index('fn vec_take_last<T>'), library.index('fn memory_copy_prefix<T>')
            raw = library[start:end]
            def pop(change):
                changed = change(raw)
                assert changed != raw
                return library[:start] + changed + library[end:] + 'fn main():\n    return\n'
            tests = [
                ('pop-underflow', pop(lambda s: s.replace('if logical_length > 0:', 'if true:')), 'W0402'),
                ('pop-wrong-index', pop(lambda s: s.replace('memory.take(&mut vector.data, index)', 'memory.take(&mut vector.data, 0)')), 'W0402'),
                ('pop-false-cell', pop(lambda s: s.replace('memory.empty(out.vector.data, out.vector.length)', 'memory.cell(out.vector.data, out.vector.length, out.item)')), 'W0402'),
                ('pop-changed-failure', pop(lambda s: s.replace('        return Err(move vector)', '        vector.length = 0\n        return Err(move vector)', 1)), 'W0402'),
                ('pop-wrong-item-unused', pop(lambda s: s.replace('input: Vec<T>)', 'input: Vec<T>, fallback: T)').replace('return Ok(VecTaken(move vector, item))', 'return Ok(VecTaken(move vector, fallback))')).replace(library[library.index('// Drop trailing items'):], ''), 'W0402'),
            ]
        if a.case == 'get-negative':
            start, end = library.index('fn vec_get<T>'), library.index('fn memory_init_unchecked<T>')
            raw = library[start:end]
            def get(change):
                changed = change(raw)
                assert changed != raw
                return library[:start] + changed + library[end:] + 'fn main():\n    return\n'
            tests = [
                ('get-false-cell', get(lambda s: s.replace('memory.cell(value.data, index, item)', 'memory.empty(value.data, index)')), 'W0402'),
                ('get-wrong-error-post', get(lambda s: s.replace('code == 1', 'code == 2')), 'W0402'),
                ('get-wrong-error-body', get(lambda s: s.replace('return Err(1)', 'return Err(2)')), 'W0402'),
                ('get-no-logical-guard', get(lambda s: s.replace('if index < (*value).length:', 'if true:')), 'W0402'),
                ('get-no-initialized-guard', get(lambda s: s.replace('if memory.initialized(&(*value).data, index):', 'if true:')), 'W0402'),
                ('get-wrong-item-unused', get(lambda s: s.replace('index: U64)', 'index: U64, fallback: T)').replace('return Ok(memory_get_unchecked(&(*value).data, index))', 'return Ok(fallback)')), 'W0402'),
                ('get-fake-refusal', get(lambda s: s.replace('if index < (*value).length:', 'if false:')), 'W0402'),
            ]
        for name, text, diagnostic in tests:
            code, data, errors = compile_text(text, name)
            assert code != 0 and not data and diagnostic in errors, (name, code, errors)
            assert output.read_bytes() == reference, name + ' overwrote previous output'
            control = dict(name=name, diagnostic=diagnostic, old_output_preserved=True)
            if diagnostic == 'W0402':
                raw = Path(re.findall(r'native diagnostic raw evidence: (.+)', errors)[-1])
                lines = (raw / 'stdout').read_text().splitlines()
                assert any(reason in '\n'.join(lines) for reason in ['unsolved goals', 'simp_all made no progress', 'omega could not prove',
                    'There are no additional binders or `let` bindings in the goal to introduce'])
                frames = [json.loads(line.split(':', 1)[1]) for line in lines if line.startswith('WITCHY_RESIDENT_V2:')]
                failed = [f for f in frames if f.get('status') == 'error']
                assert failed and all(
                    any(f.get('status') == 'begin' and f.get('seq') == bad.get('seq') for f in frames)
                    and not any(f.get('status') == 'ok' and f.get('seq') == bad.get('seq') for f in frames)
                    for bad in failed)
                shutil.copytree(raw, a.out / (name + '-native'))
                control.update(actual_kernel_failure=True, failed_frames=failed)
            report['controls'].append(control)
            save()
        execute('preserved-wasmtime')
        report['old_output_preserved'] = True
    report['status'] = 'passed'
    save()
except BaseException as error:
    report.update(status='failed', error=repr(error))
    save()
    raise
