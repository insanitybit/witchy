#!/usr/bin/env python3
"""Focused actual Stack proofs/refusals and Wasmtime execution; reserve host first."""
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
p.add_argument('--case', choices=['positive', 'negative', 'outcomes'], required=True,
               help='Stack proof/run, selected refusals, or independent checked-Take outcomes')
p.add_argument('--reference', type=Path)
p.add_argument('--only', action='append', help='Named negative controls only')
a = p.parse_args()
repo = Path(__file__).resolve().parents[1]
assert shutil.disk_usage(a.out.parent).free >= 10 * 2**30, '10 GiB launch floor'
a.out.mkdir()
source = (repo / 'examples/generic-proof-stack.wy').read_text()
fixtures = repo / 'tests/fixtures/generic-proof-stack'
outcomes_source = source[:source.index('// Copyable results')] + (fixtures / 'take-outcomes.wy').read_text()
if a.case == 'outcomes':
    source = outcomes_source
program, output = a.out / 'program.wy', a.out / 'program.wasm'
env = {k: v for k, v in os.environ.items()
       if not k.startswith(('WITCHY_', 'LEAN_', 'LAKE_', 'NODE_', 'DYLD_', 'LD_'))}
env.update(WITCHY_WASM_READY_BINDING=str(a.binding),
           WITCHY_WASMTIME_HOST=str(a.runtime))
report = dict(status='running', case=a.case, commands=[], controls=[],
              source_sha256=sha(source.encode()), compiler_sha256=sha(a.compiler.read_bytes()))


def save():
    pending = a.out / 'results.pending'
    pending.write_text(json.dumps(report, indent=2) + '\n')
    pending.replace(a.out / 'results.json')


def invoke(args, name):
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
    expected = [73, 37, 9, 7, 0, 1, 9, 4294967296, 7, 18446744073709551615,
                7, 0, 0, 99, 1, 11, 0, 17, 1, 0, 31, 99, 99,
                18446744073709551615, 99, 0, 41, 1, 80]
    if a.case == 'outcomes':
        expected = [1, 73, 73, 2, 1]
    assert code == 0 and not errors and data.decode() == ''.join(f'{v}\n' for v in expected), (code, data, errors)
    report['runtime_output_lines'] = len(expected)


def changed_function(name, change):
    start = source.index('fn ' + name + '<')
    end = source.index('\nfn ', start + 1)
    original = source[start:end]
    changed = change(original)
    assert original != changed
    return source[:start] + changed + source[end:]


try:
    save()
    if a.case in ['positive', 'outcomes']:
        code, data, errors = compile_text(source, 'positive')
        assert code == 0 and not errors, (code, errors)
        receipt = json.loads(data)
        assert receipt['source_sha256'] == sha(program.read_bytes())
        assert receipt['wasm_sha256'] == sha(output.read_bytes())
        assert receipt['proof_admission'] is False
        obligations = receipt['obligations']
        assert all(g['outcome'] == 'fresh_kernel_checked' for g in obligations)
        for name in ['stack_push', 'stack_pop', 'stack_clear', 'stack_buffer_close', 'stack_close']:
            assert any(g['function'] == name and g['goal_kind'] == 'function_body' for g in obligations)
        if a.case == 'positive':
            for name in ['stack_push', 'stack_pop', 'stack_close']:
                assert any(g['function'] == 'stack_roundtrip' and g.get('callee') == name for g in obligations)
        else:
            assert any(g['function'] == 'extract_at' and g['goal_kind'] == 'function_body' for g in obligations)
            assert any(g['function'] == 'main' and g.get('callee') == 'extract_at' for g in obligations)
        assert any(g.get('source_model') == 'witchy.checked-storage-record.v5' for g in obligations)
        report['obligations'] = obligations
        report['selected_generation_kernel_checks'] = receipt['selected_generation_kernel_checks']
        report['wasm_sha256'] = receipt['wasm_sha256']
        execute('wasmtime')
    else:
        assert a.reference and a.reference.is_file()
        reference = a.reference.read_bytes()
        output.write_bytes(reference)
        definitions = source[:source.index('// Copyable results')]
        wrong_item = definitions.replace('fn stack_pop<T>(input: Stack<T>)',
                    'fn stack_pop<T>(input: Stack<T>, fallback: T)')
        wrong_item = wrong_item.replace('return Ok(StackTaken(move stack, item))',
                                        'return Ok(StackTaken(move stack, fallback))') + 'fn main():\n    return\n'
        unknown_index = changed_function('stack_pop', lambda s: s.replace(
            'memory.take(&mut stack.slots, index)', 'memory.take(&mut stack.slots, opaque_index(index))'))
        unknown_index += '\nfn opaque_index(index: U64) -> U64:\n    return index\n'
        tests = [
            ('missing-proof', changed_function('stack_push', lambda s: s.replace('    proof:\n        auto\n', '')), 'W0401'),
            ('false-push-length', changed_function('stack_push', lambda s: s.replace('out.used == old(input.used) + 1', 'out.used == old(input.used) + 2')), 'W0402'),
            ('raw-out-of-bounds', changed_function('stack_push', lambda s: s.replace('memory.init_unchecked(&mut stack.slots, index, item)', 'memory.init_unchecked(&mut stack.slots, actual, item)')), 'W0402'),
            ('raw-occupied-slot', changed_function('stack_push', lambda s: s.replace('if !memory.initialized(&stack.slots, index):', 'if true:')), 'W0402'),
            ('pop-underflow', changed_function('stack_pop', lambda s: s.replace('if stack.used > 0:', 'if true:')), 'W0402'),
            ('pop-wrong-index', changed_function('stack_pop', lambda s: s.replace('memory.take(&mut stack.slots, index)', 'memory.take(&mut stack.slots, 0)')), 'W0402'),
            ('pop-wrong-item-unused', wrong_item, 'W0402'),
            ('pop-false-cell', changed_function('stack_pop', lambda s: s.replace('memory.empty(out.stack.slots, out.stack.used)', 'memory.cell(out.stack.slots, out.stack.used, out.item)')), 'W0402'),
            ('pop-changed-failure', changed_function('stack_pop', lambda s: s.replace('        return Err(move stack)', '        stack.used = 0\n        return Err(move stack)', 1)), 'W0402'),
            ('unverified-index-call', unknown_index, 'W0402'),
            ('duplicate-owner', source + '\nfn duplicate<T>(input: Stack<T>):\n    stack_close(move input)\n    stack_close(move input)\n', 'W0401'),
            ('shared-write', source + '\nfn shared_write<T>(slots: &Buffer<T>, item: T):\n    memory.init_unchecked(slots, 0, item)\n', 'W0401'),
            ('take-false-bounds-code', outcomes_source.replace('back.code == 1', 'back.code == 2', 1), 'W0402'),
            ('take-false-empty-code', outcomes_source.replace('back.code == 2', 'back.code == 1', 1), 'W0402'),
            ('live-borrow-release', source + '\n' + (fixtures / 'live-borrow-release.wy').read_text(), 'W0401'),
            ('owning-element', source + '\n' + (fixtures / 'owning-element.wy').read_text(), 'W0403'),
        ]
        if a.only:
            assert set(a.only) <= {name for name, _, _ in tests}
            tests = [case for case in tests if case[0] in a.only]
        for name, text, diagnostic in tests:
            code, data, errors = compile_text(text, name)
            assert code != 0 and not data and diagnostic in errors, (name, code, errors)
            if name == 'live-borrow-release':
                assert 'borrow' in errors or 'loan' in errors, errors
            if name == 'owning-element':
                assert 'storage' in errors.lower() or 'copy' in errors.lower(), errors
            assert output.read_bytes() == reference, name + ' overwrote previous output'
            control = dict(name=name, diagnostic=diagnostic, old_output_preserved=True)
            if diagnostic == 'W0402':
                raw = Path(re.findall(r'native diagnostic raw evidence: (.+)', errors)[-1])
                lines = (raw / 'stdout').read_text().splitlines()
                assert any(reason in '\n'.join(lines) for reason in ['unsolved goals', 'simp_all made no progress', 'omega could not prove'])
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
