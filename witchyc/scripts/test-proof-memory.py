#!/usr/bin/env python3
"""Focused real Witchy/Lean memory proofs and Wasmtime execution; exclusive host only."""
import argparse, hashlib, json, os, re, shutil, signal, subprocess, time
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--compiler', type=Path, required=True)
p.add_argument('--binding', type=Path, required=True)
p.add_argument('--runtime', type=Path, required=True)
p.add_argument('--out', type=Path, required=True)
p.add_argument('--case', choices=['positive', 'negative'], required=True)
p.add_argument('--suite', choices=['get', 'init', 'ordinary', 'migration', 'cleanup', 'reserve'], default='get')
p.add_argument('--reference', type=Path)
p.add_argument('--only', action='append', help='Run named negative controls when resuming focused validation')
a = p.parse_args()
signal.signal(signal.SIGTERM, signal.default_int_handler)
assert shutil.disk_usage(a.out.parent).free >= 10 * 2**30, '10 GiB launch floor'
a.out.mkdir()
repo = Path(__file__).resolve().parents[1]
library = (repo/'library/wasm-vec.wy').read_text()
client = (repo/('tests/fixtures/proof-memory-cleanup.wy' if a.suite == 'cleanup' else 'tests/fixtures/vec-reserve.wy' if a.suite == 'reserve' else 'tests/fixtures/vec-migration.wy' if a.suite == 'migration' else 'tests/fixtures/ordinary-proof-vec.wy' if a.suite == 'ordinary' else 'tests/fixtures/proof-memory-init.wy' if a.suite == 'init' else 'tests/fixtures/proof-memory-get.wy')).read_text()
if a.suite == 'cleanup':
    assert a.case == 'positive'
    # The fixture carries its own tail-scan helper; the library no longer needs one.
    library = library[library.index('fn memory_clear<T>'):library.index('fn vec_buffer_close<T>')]
source, output = a.out/'program.wy', a.out/'program.wasm'
env = dict(PATH=os.environ.get('PATH', '/usr/bin:/bin'), WITCHY_WASM_READY_BINDING=str(a.binding),
           WITCHY_WASMTIME_HOST=str(a.runtime))
report = dict(status='running', case=a.case, commands=[])
def sha(data): return hashlib.sha256(data).hexdigest()
def save():
    tmp = a.out/'results.pending'; tmp.write_text(json.dumps(report, indent=2)+'\n'); tmp.replace(a.out/'results.json')
def absent(pid):
    try: os.kill(pid, 0)
    except ProcessLookupError: return True
    return False
def invoke(args, name):
    stdout, stderr = a.out/(name+'.stdout'), a.out/(name+'.stderr')
    helpers = set(); child = None; started = time.monotonic()
    with stdout.open('wb') as out, stderr.open('wb') as err:
        try:
            child = subprocess.Popen(list(map(str, args)), env=env, stdin=subprocess.DEVNULL,
                                     stdout=out, stderr=err, start_new_session=True)
            while child.poll() is None:
                assert shutil.disk_usage(a.out).free >= 4 * 2**30, '4 GiB live floor'
                assert time.monotonic()-started < 40, 'focused command deadline'
                assert stdout.stat().st_size < 2**20 and stderr.stat().st_size < 2**20, 'output bound'
                rows = subprocess.check_output(['/bin/ps', '-axo', 'pid=,ppid='], text=True)
                for pid, parent in map(str.split, rows.splitlines()):
                    if int(parent) == child.pid: helpers.add(int(pid))
                time.sleep(.02)
            child.wait(timeout=5)
        finally:
            if child is not None:
                if child.poll() is None: os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=5)
                assert all(absent(pid) for pid in helpers), 'owned helper unresolved: reservation must stay held'
    row = dict(name=name, exit=child.returncode, seconds=time.monotonic()-started,
               pid=child.pid, helpers=sorted(helpers), helpers_absent=True)
    report['commands'].append(row); save()
    return child.returncode, stdout.read_bytes(), stderr.read_text()
def compile_text(text, name):
    source.write_text(text)
    (a.out/(name+'.wy')).write_text(text)
    return invoke([a.compiler, 'wasm-checked', source, '-o', output], name)
def execute(name):
    code, data, errors = invoke([a.runtime, output, '--output', '--allocator'], name)
    expected = '7\n10\n11\n12\n13\n14\n18446744073709551615\n1\n9\n4294967296\n15\n16\n'
    if a.suite == 'init':
        values = [10] + [7]*10 + [99,10,20,9,0,5,1,9,9,1,0,9,18446744073709551615,2,18446744073709551615,4294967296,1,0,9,4294967296,11,18446744073709551615,9,0,123,1,123,0,30]
        expected = ''.join(f'{value}\n' for value in values)
    if a.suite in ('ordinary', 'migration', 'reserve'):
        values = [3,4,7,9,11,1,3,11,18446744073709551615,4294967296,17,1,0,1,7,4294967296,9,18446744073709551615,11,17,9,1,1,7,9,0,1,7,9,0,99,404,9,18446744073709551615,1,404,9,0,0,404]
        if a.suite in ('migration', 'reserve'):
            values += [4,7,11,7,404,3,7,99,404,9,2,404,1,404,0,3,3,7,1,11,12,16] + [7]*12
        if a.suite == 'reserve':
            values += [80,0,7,11,1,1]
        expected = ''.join(f'{value}\n' for value in values)
    if a.suite == 'cleanup':
        expected = '1\n0\n0\n1\n'
    assert code == 0 and data.decode() == expected and not errors, (code, data, errors)
try:
    save()
    if a.case == 'positive':
        code, data, errors = compile_text(library+client, 'positive')
        assert code == 0 and not errors, (code, errors)
        receipt = json.loads(data)
        assert receipt['source_sha256'] == sha(source.read_bytes())
        assert receipt['wasm_sha256'] == sha(output.read_bytes())
        memory = [g for g in receipt['obligations'] if g.get('source_model') == 'witchy.proof-required-memory.v4']
        assert memory
        if a.suite == 'cleanup':
            assert any(g['function'] == 'memory_clear' and g['goal_kind'] == 'function_body' for g in memory)
            assert any(g['function'] == 'memory_check_empty_from' and g['goal_kind'] == 'function_body' for g in memory)
        else:
            assert any(g['function'] == 'memory_get_unchecked' for g in memory)
            assert any(g.get('callee') == 'memory_get_unchecked' and g['function'] == 'vec_get' for g in memory)
        if a.suite == 'init':
            assert any(g['function'] == 'memory_init_unchecked' and g['goal_kind'] == 'function_body' for g in memory)
            assert any(g.get('callee') == 'memory_init_unchecked' and g['function'] == 'vec_push_spare_mut' for g in memory)
            assert any(g.get('callee') == 'memory_get_unchecked' and g['function'] == 'init_and_read_if_empty' for g in memory)
        if a.suite in ('ordinary', 'migration', 'reserve'):
            records = [g for g in receipt['obligations'] if g.get('source_model') == 'witchy.checked-storage-record.v5']
            assert any(g['function'] == 'vec_push_spare' and g['goal_kind'] == 'function_body' and g['outcome'] == 'fresh_kernel_checked' for g in records)
            assert any(g['function'] == 'vec_push' and g.get('callee') == 'vec_push_spare' for g in records)
            report['record_obligations'] = records
        if a.suite == 'migration':
            assert any(g['function'] == 'memory_copy_prefix' and g['goal_kind'] == 'function_body' for g in memory)
            assert any(g['function'] == 'vec_reserve' and g.get('callee') == 'memory_copy_prefix' for g in memory)
        if a.suite == 'reserve':
            assert all(any(g['function'] == function and g['goal_kind'] == 'function_body' and g['outcome'] == 'fresh_kernel_checked' for g in records) for function in ['vec_reserve','vec_buffer_close','vec_close'])
        checks = receipt['selected_generation_kernel_checks']
        assert checks['source_sha256'] == sha(source.read_bytes())
        assert len(checks['audited_dependencies']) == len(receipt['obligations'])
        assert all(g['outcome'] == 'fresh_kernel_checked' for g in memory)
        report['memory_obligations'] = memory
        report['source_sha256'] = sha(source.read_bytes())
        report['wasm_sha256'] = sha(output.read_bytes())
        execute('actual-wasmtime')
    else:
        assert a.reference and a.reference.is_file()
        shutil.copy2(a.reference, output)
        reference = output.read_bytes()
        start, end = library.index('fn memory_get_unchecked'), library.index('fn vec_get')
        raw = library[start:end]
        def replace_raw(changed): return library[:start]+changed+library[end:]+client
        tests = [
            ('missing-proof', replace_raw(raw.replace('    proof:\n        auto\n', '')), 'W0401'),
            ('missing-initialization', library.replace(' && memory.initialized(buffer, index)', '', 1)+client, 'W0402'),
            ('wrong-access-bounds', replace_raw(raw.replace('return memory.get_unchecked(buffer, index)', 'return memory.get_unchecked(buffer, memory.capacity(buffer))')), 'W0402'),
            ('false-postcondition', replace_raw(raw.replace('ensures memory.cell(buffer, index, result)', 'ensures false')), 'W0402'),
            ('missing-safe-guard', library.replace('            if memory.initialized(&(*value).data, index):\n                return Ok(memory_get_unchecked(&(*value).data, index))', '            return Ok(memory_get_unchecked(&(*value).data, index))')+client, 'W0402'),
            ('wrong-safe-call-index', library.replace('return Ok(memory_get_unchecked(&(*value).data, index))', 'return Ok(memory_get_unchecked(&(*value).data, index + 1))')+client, 'W0402'),
            ('unused-unproved-access', library+'fn unproved<T>(buffer: &Buffer<T>, index: U64) -> T:\n    return memory.get_unchecked(buffer, index)\n'+client, 'W0401'),
            ('dead-unproved-access', library+'fn dead_unproved<T>(buffer: &Buffer<T>, index: U64, item: T) -> T:\n    if false:\n        memory.get_unchecked(buffer, index)\n    return item\n'+client, 'W0401'),
            ('mutable-proof-context', library+'fn mutable_read<T>(buffer: &mut Buffer<T>, index: U64, item: T) -> T:\n    requires index < memory.capacity(buffer)\n    requires memory.initialized(buffer, index)\n    ensures true\n    proof:\n        auto\n    memory.swap(buffer, index, item)\n    return memory.get_unchecked(buffer, index)\n'+client, 'W0401'),
            ('owned-payload', library+'fn owner_payload(buffer: Buffer<Buffer<U8>>):\n    return\n'+client, 'W0403'),
        ]
        if a.suite == 'init':
            start, end = library.index('fn memory_init_unchecked'), library.index('fn vec_buffer_close')
            raw = library[start:end]
            def init_changed(changed): return library[:start]+changed+library[end:]+client
            wrong_payload = raw.replace('fn memory_init_unchecked<T>', 'fn wrong_payload<T>').replace('item: T):', 'item: T, other: T):').replace('memory.init_unchecked(buffer, index, item)', 'memory.init_unchecked(buffer, index, other)')
            shared = raw.replace('fn memory_init_unchecked<T>', 'fn shared_write<T>').replace('buffer: &mut Buffer<T>', 'buffer: &Buffer<T>')
            tests = [
                ('init-missing-proof', init_changed(raw.replace('    proof:\n        auto\n', '', 1)), 'W0401'),
                ('init-missing-bounds', init_changed(raw.replace('    requires index < memory.capacity(buffer)\n', '', 1)), 'W0402'),
                ('init-missing-empty', init_changed(raw.replace('    requires memory.empty(buffer, index)\n', '', 1)), 'W0402'),
                ('init-wrong-index', init_changed(raw.replace('memory.init_unchecked(buffer, index, item)', 'let write_index = memory.capacity(buffer)\n    memory.init_unchecked(buffer, write_index, item)')), 'W0402'),
                ('init-wrong-payload', library+wrong_payload+client, 'W0402'),
                ('init-wrong-frame', init_changed(raw.replace('memory.unchanged_except(buffer, old(buffer), index)', 'memory.unchanged_except(buffer, old(buffer), index + 1)')), 'W0402'),
                ('init-twice', init_changed(raw.replace('    return\n', '    memory.init_unchecked(buffer, index, item)\n    return\n', 1)), 'W0402'),
                ('init-weak-post', init_changed(raw.replace('    ensures memory.cell(buffer, index, item)\n', '', 1)), 'W0402'),
                ('init-unused-unproved', library+'fn unproved_init<T>(buffer: &mut Buffer<T>, index: U64, item: T):\n    memory.init_unchecked(buffer, index, item)\n'+client, 'W0401'),
                ('init-shared-permission', library+shared+client, 'W0401'),
                ('init-owned-payload', library+'fn owner_payload(buffer: Buffer<Buffer<U8>>):\n    return\n'+client, 'W0403'),
            ]
        if a.suite == 'ordinary':
            start, end = library.index('fn vec_push_spare<T>'), library.index('fn vec_take_last<T>')
            raw = library[start:end]
            def ordinary_changed(changed): return library[:start]+changed+library[end:]+client
            wrong_item = raw.replace('fn vec_push_spare<T>', 'fn wrong_return<T>').replace('item: T) ->', 'item: T, other: T) ->').replace('return Err(VecPushError(move vector, item))', 'return Err(VecPushError(move vector, other))')
            wrong_stored = raw.replace('fn vec_push_spare<T>', 'fn wrong_store<T>').replace('item: T) ->', 'item: T, other: T) ->').replace('memory.init_unchecked(&mut vector.data, index, item)', 'memory.init_unchecked(&mut vector.data, index, other)')
            duplicated = 'fn duplicate<T>(input: Vec<T>, item: T) -> Result<Vec<T>, VecPushError<T>>:\n    let output = vec_push_spare(move input, item)\n    vec_close(move input)\n    return move output\n'
            tests = [
                ('ordinary-missing-proof', ordinary_changed(raw.replace('    proof:\n        auto\n', '')), 'W0401'),
                ('ordinary-no-empty-guard', ordinary_changed(raw.replace('if !memory.initialized(&vector.data, index):', 'if true:')), 'W0402'),
                ('ordinary-wrong-index', ordinary_changed(raw.replace('memory.init_unchecked(&mut vector.data, index, item)', 'memory.init_unchecked(&mut vector.data, actual_capacity, item)')), 'W0402'),
                ('ordinary-wrong-length', ordinary_changed(raw.replace('vector.length = index + 1', 'vector.length = index + 0')), 'W0402'),
                ('ordinary-wrong-return-item', library+wrong_item+client, 'W0402'),
                ('ordinary-wrong-stored-item', library+wrong_stored+client, 'W0402'),
                ('ordinary-duplicate-owner', library+duplicated+client, 'W0401'),
                ('ordinary-unused-unsafe', library+'fn unproved<T>(buffer: &mut Buffer<T>, index: U64, item: T):\n    memory.init_unchecked(buffer, index, item)\n    return\n'+client, 'W0401'),
            ]
        if a.suite == 'migration':
            start, end = library.index('fn memory_copy_prefix<T>'), library.index('fn vec_reserve<T>')
            raw = library[start:end]
            def migration_changed(changed): return library[:start]+changed+library[end:]+client
            no_invariants = '\n'.join(line for line in raw.split('\n') if not line.strip().startswith('invariant '))
            alias = 'fn alias<T>(input: Buffer<T>, count: U64):\n    var buffer = move input\n    memory_copy_prefix(&buffer, &mut buffer, count)\n    vec_buffer_close(move buffer)\n'
            unproved = 'fn unproved_loop():\n    var index: U64 = 0\n    while index < 1:\n        invariant index <= 1\n        index += 1\n'
            tests = [
                ('migration-missing-invariant', migration_changed(no_invariants), 'W0401'),
                ('migration-false-initial-invariant', migration_changed(raw.replace('invariant index <= count', 'invariant index == 1')), 'W0402'),
                ('migration-unpreserved-prefix', migration_changed(raw.replace('                memory.init_unchecked(target, index, item)\n', '')), 'W0402'),
                ('migration-missing-source-guard', migration_changed(raw.replace('if memory.initialized(source, index):', 'if true:')), 'W0402'),
                ('migration-missing-target-guard', migration_changed(raw.replace('if !memory.initialized(target, index):', 'if true:')), 'W0402'),
                ('migration-fake-success', migration_changed(raw.replace('return Err(4)', 'return Ok(())')), 'W0402'),
                ('migration-aliasing-owner', library+alias+client, 'W0401'),
                ('migration-unproved-annotation', library+unproved+client, 'W0401'),
                ('migration-false-source-frame', migration_changed(raw.replace('    ensures memory.unchanged(source, old(source))', '    ensures memory.frame_unchanged(old(source))')), 'W0401'),
            ]
        if a.suite == 'reserve':
            start, end = library.index('fn vec_reserve<T>'), library.index('fn vec_push<T>')
            raw = library[start:end]
            def reserve_changed(changed): return library[:start]+changed+library[end:]+client
            reuse = 'fn reused<T>(input: Vec<T>) -> Vec<T>:\n    vec_close(move input)\n    return move input\n'
            tests = [
                ('reserve-fake-allocation-success', reserve_changed(raw.replace('        Err(allocation_error):\n            return Err(move input)', '        Err(allocation_error):\n            return Ok(move input)')), 'W0402'),
                ('reserve-wrong-copy-count', reserve_changed(raw.replace('memory_copy_prefix(&input.data, &mut new_buffer, logical_length)', 'memory_copy_prefix(&input.data, &mut new_buffer, 0)')), 'W0402'),
                ('reserve-changed-failure-header', reserve_changed(raw.replace('return Err(move input)', 'return Err(Vec(move input.data, 0, claimed_capacity))', 1)), 'W0402'),
                ('reserve-weak-copy-post', library.replace('            memory.same_prefix(target, source, count)\n', '            true\n', 1)+client, 'W0402'),
                ('reserve-weak-clear-post', library.replace('    ensures memory.empty_all(buffer)\n', '    ensures true\n', 1)+client, 'W0402'),
                ('reserve-skip-clear-take', library.replace('        memory.take(buffer, index)\n', '', 1)+client, 'W0402'),
                ('reserve-missing-old-cleanup', reserve_changed(raw.replace('                            vec_close(move input)\n', '', 1)), 'W0401'),
                ('reserve-reuse-closed-owner', library+reuse+client, 'W0401'),
            ]
        if a.only:
            assert set(a.only) <= {name for name, _, _ in tests}, 'unknown negative control'
            tests = [test for test in tests if test[0] in a.only]
        for name, text, diagnostic in tests:
            assert text != library+client
            code, data, errors = compile_text(text, name)
            assert code != 0 and not data and diagnostic in errors, (name, code, errors)
            if name == 'ordinary-duplicate-owner':
                assert 'consumed or maybe-moved' in errors, errors
            assert output.read_bytes() == reference, name+' overwrote old output'
            if diagnostic == 'W0402':
                evidence = Path(re.findall(r'native diagnostic raw evidence: (.+)', errors)[-1])
                raw_error = (evidence/'stdout').read_text()
                assert any(reason in raw_error for reason in ['unsolved goals', 'simp_all made no progress', 'omega could not prove']), raw_error
                assert any('"status":"error"' in line for line in raw_error.splitlines() if line.startswith('WITCHY_RESIDENT_V2:'))
                shutil.copytree(evidence, a.out/(name+'-native'))
        execute('preserved-output-wasmtime')
        report['old_output_preserved'] = True
    report['status'] = 'passed'; save()
except BaseException as error:
    report.update(status='failed', error=repr(error)); save(); raise
