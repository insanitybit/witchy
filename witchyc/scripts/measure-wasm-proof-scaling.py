#!/usr/bin/env python3
"""Bounded full-recheck observations; acquire the coordinator host lease first.

Use already-built optimized binaries. Fresh means absent output, never cold FS.
No build, optimization, cache/admission reuse, or proof rejection measurement.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import random
import shutil
import signal
import statistics
import sys
import threading
import time


def distribution(values):
    result = dict(n=len(values), median=statistics.median(values), minimum=min(values), maximum=max(values))
    if len(values) >= 12:
        rng = random.Random(0)
        boot = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(2000))
        result['median_bootstrap_95'] = [boot[50], boot[1949]]
    return result


def source(case, edited, vec):
    if case.endswith('-proof'):
        text, expected, goals = source(case[:-6], False, vec)
        if edited:
            if case == 'vec-proof':
                claim = '            output.length == old(input.length) + 1\n'
                assert text.count(claim) == 1, 'canonical Vec Ok post required'
                text = text.replace(claim, claim + '            output.length > old(input.length)\n', 1)
            else:
                assert text.count('    ensures x == x\n') == 1
                text = text.replace('    ensures x == x\n', '    ensures x <= 18446744073709551615\n', 1)
        return text, expected, goals
    value = 2 if edited else 1
    if case == 'vec':
        before, main = vec.split('fn main(', 1)
        target = 'exercise(&console, &allocator, byte, 2)'
        assert main.count(target) == 1 and 'exercise<T>' in before and 'requires memory.prefix' in before
        if edited:
            main = main.replace(target, target[:-2] + '3)', 1)
        return before + 'fn main(' + main, ('0\n1\n3\n1\n' if edited else '0\n1\n2\n1\n') + '0\n1\n2\n1\n' * 3 + '1\n0\n0\n', 2
    size = int(case.split('-')[1])
    if case.startswith('ghost-'):
        text = ''.join(f'lemma witness{i}(x: U64):\n    ensures x == x\n    proof:\n        auto\n' for i in range(size))
        return text + f'fn main(console: Console<Write>):\n    console.print_int({value})\n', f'{value}\n', size
    text = 'type Box<T>:\n    value: T\n'
    for i in range(size):
        operand = 'x' if i == 0 else f'f{i-1}(x)'
        text += f'fn f{i}(x: Box<U64>) -> Box<U64>:\n    return Box({operand}.value + {value if i == 0 else 1})\n'
    return text + f'fn main(console: Console<Write>):\n    console.print_int(f{size-1}(Box(0)).value)\n', f'{size - 1 + value}\n', 0


def phase_times(measured):
    result = {key: measured[key] for key in ['read', 'parse', 'static_check', 'fresh_proof_check', 'emit', 'receipt', 'publish']}
    for family, fields in measured.get('phases', {}).items():
        result.update({f'{family}.{key}': value for key, value in fields.items() if key != 'counts'})
    return result


def claim_fingerprint(report):
    claims = []
    for goal in report['obligations']:
        assert goal['outcome'] == 'fresh_kernel_checked'
        assert hashlib.sha256(goal['expected_statement'].encode()).hexdigest() == goal['statement_sha256']
        claims.append([goal['theorem'], goal['statement_sha256']])
    return hashlib.sha256(json.dumps(claims, separators=(',', ':')).encode()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True, help='the Witchy runtime, tools/wasmtime-host')
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--compiler-revision', help='operator-declared binary source revision')
    parser.add_argument('--baseline-revision', help='operator-declared baseline source revision')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--lean-root', type=Path, required=True)
    parser.add_argument('--vec-source', type=Path)
    choices = ['ghost-1', 'ghost-8', 'ghost-32', 'chain-16', 'chain-64', 'chain-120', 'vec', 'ghost-1-proof', 'vec-proof']
    parser.add_argument('--cases', nargs='+', choices=choices)
    parser.add_argument('--states', nargs='+', choices=['fresh', 'unchanged', 'edit'], default=['fresh', 'unchanged', 'edit'])
    parser.add_argument('--samples', type=int, default=3)
    parser.add_argument('--max-seconds', type=int, default=1800)
    args = parser.parse_args()
    if sys.flags.optimize:
        parser.error('Python assertion checks must remain enabled')
    root, out = Path(__file__).resolve().parents[1], args.out.resolve()
    cases = args.cases or ['ghost-1', 'ghost-8'] + (['vec'] if args.vec_source else [])
    phases = args.states
    if not 3 <= args.samples <= 24 or not 60 <= args.max_seconds <= 3600:
        parser.error('samples must be 3..24; campaign limit 60..3600 seconds')
    if len(cases) != len(set(cases)) or len(phases) != len(set(phases)) or (any(case.startswith('vec') for case in cases) and not args.vec_source):
        parser.error('unique cases/states and --vec-source for vec required')
    if root == out or root in out.parents or not args.lean_root.is_absolute():
        parser.error('output must be outside checkout; trusted Lean root must be absolute')
    versions = {'candidate': args.compiler.resolve()}
    if args.baseline:
        versions['baseline'] = args.baseline.resolve()
    if not all(path.is_file() and os.access(path, os.X_OK) for path in versions.values()):
        parser.error('already-built executable binaries required')
    spec = importlib.util.spec_from_file_location('isolated', root / 'scripts/harness.py')
    isolated = importlib.util.module_from_spec(spec)
    sys.dont_write_bytecode = True
    spec.loader.exec_module(isolated)
    pins = isolated.snapshot(root, isolated.tracked(root))
    compiler_pins = {key: isolated.digest(path) for key, path in versions.items()}
    fixture = args.vec_source.resolve() if args.vec_source else None
    fixture_pin = isolated.digest(fixture) if fixture else None
    vec = fixture.read_text() if fixture else None
    for case in cases:
        assert source(case, False, vec)[0] != source(case, True, vec)[0]
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    env = {key: value for key, value in os.environ.items() if key not in ['LEAN', 'LAKE'] and not key.startswith(
        ('WITCHY_', 'LEAN_', 'LAKE_', 'NODE_', 'DYLD_', 'LD_'))}
    env['WITCHY_WASM_LEAN_ROOT'] = str(args.lean_root)
    (out / 'tmp').mkdir(mode=0o700)
    env['TMPDIR'] = str(out / 'tmp')
    runtime = args.runtime.resolve()
    case_modes = {case: 'authored-proof-edit' if case.endswith('-proof') else 'runtime-code-edit' for case in cases}
    state = dict(status='running', case_modes=case_modes, samples=args.samples, cases=cases, states=phases, seed=0, rows=[], setup=[], commands=[],
        platform=platform.platform(), machine=platform.machine(), python=platform.python_version(), pid=os.getpid(),
        compiler_pins=compiler_pins, compilers={key: str(path) for key, path in versions.items()},
        declared_revisions={'candidate': args.compiler_revision, 'baseline': args.baseline_revision},
        fixture=str(fixture) if fixture else None, fixture_sha256=fixture_pin, lean_root=str(args.lean_root),
        harness_sha256=isolated.digest(Path(__file__)), max_seconds=args.max_seconds,
        exclusions=['no cold-filesystem/cache-hit claim', 'no kernel-only timing', 'no hardware counters',
                    'no allocation/hash-read volume instrumentation', 'no rejection-latency or speedup claim'])
    finished = threading.Event()
    deadline = time.monotonic() + args.max_seconds

    def save():
        temporary = out / 'results.pending'
        temporary.write_text(json.dumps(state, indent=2) + '\n')
        temporary.replace(out / 'results.json')

    def stop(signum, frame):
        isolated.CANCEL.set()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    def watch():
        while not finished.wait(.25):
            if isolated.CANCEL.is_set() or shutil.disk_usage(out).free < 4 * 2**30 or time.monotonic() >= deadline:
                state['cancelled'] = True
                isolated.cancel_commands()
                return
    watcher = threading.Thread(target=watch)

    def call(argv, label, timeout=90):
        assert shutil.disk_usage(out).free >= 16 * 2**30, '16 GiB launch guard'
        remaining = deadline - time.monotonic()
        assert remaining > 0 and not isolated.CANCEL.is_set(), 'cancelled or campaign limit'
        result, seconds = isolated.command(argv, root, env, out / f'{label}.log',
            timeout=min(timeout, remaining), separate_stderr=True)
        state['commands'].append(dict(label=label, argv=list(map(str, argv)), seconds=seconds, exit=result.returncode))
        assert result.returncode == 0 and not isolated.CANCEL.is_set(), label
        return result.stdout, seconds


    def consume(version, case, edited, label, output, measured=True):
        text, expected, goals = source(case, edited, vec)
        input_path = out / f'{label}.wy'
        input_path.write_text(text)
        assert isolated.digest(versions[version]) == compiler_pins[version], 'compiler pin changed'
        receipt_mode = not measured or case.endswith('-proof')
        mode = 'wasm-checked' if receipt_mode else 'wasm-measure'
        result, seconds = call([versions[version], mode, input_path, '-o', output], label)
        report = json.loads(result)
        if not receipt_mode:
            assert report['debug_assertions'] is False and report['lean_invocations'] == int(goals > 0)
            proof = report.get('phases', {}).get('fresh_proof_check')
            if proof is not None:
                counts = proof['counts']
                assert counts['selected_goals'] == goals
                assert counts['compiler_hashes'] == counts['installation_snapshots'] == (2 if goals else 0)
                if not goals:
                    assert counts['generated_module_bytes'] == proof['child_spawn_to_exit'] == 0
                assert sum(value for key, value in proof.items() if key != 'counts') <= report['fresh_proof_check']
                assert sum(report['phases']['static_check'].values()) <= report['static_check']
        else:
            assert report['all_selected_checked'] and report['proof_admission'] is False
            assert len(report['obligations']) == goals
            fingerprint = claim_fingerprint(report)
            if goals:
                fresh = report['fresh_kernel_checks']
                assert fresh['admission_reuse'] is False and len(fresh['audited_dependencies']) == goals
                assert fresh['source_sha256'] == hashlib.sha256(text.encode()).hexdigest()
                assert fresh['compiler_sha256'] == compiler_pins[version]
                identity = {key: fresh[key] for key in ['lean_version', 'lean_installation_sha256']}
                assert not state.get('proof_tools') or identity in state['proof_tools'].values(), 'proof tool drift'
                state.setdefault('proof_tools', {})[version] = identity
            if case in ['vec', 'vec-proof']:
                assert {(g['goal_kind'], g['function']) for g in report['obligations']} == {('function_body', 'vec_push_spare'), ('call_precondition', 'exercise')}
                for name in ['exercise', 'vec_push_spare']:
                    assert sum(item['definition'] == name for item in report['function_instances']) == 4
            if case.startswith('chain-'):
                assert len(report['selected_functions']) == int(case.split('-')[1]) + 1
        host_text, host_seconds = call([runtime, output, '--output', '--allocator'], label + '-host', 10)
        host = dict(status='ok', output=host_text)
        assert host['status'] == 'ok' and host['output'] == expected, host
        return dict(label=label, expected_output=expected, source_sha256=hashlib.sha256(text.encode()).hexdigest(), source_bytes=len(text.encode()),
            wasm_sha256=isolated.digest(output), compiler_sha256=compiler_pins[version], fixture_sha256=fixture_pin,
            compiler_seconds=seconds, compiler_mode=mode, case_mode=case_modes.get(case, 'runtime-code-edit'),
            compiler_phases=None if receipt_mode else report, phase_ns={} if receipt_mode else phase_times(report),
            compiler_receipt=report if receipt_mode else None, claim_fingerprint=fingerprint if receipt_mode else None,
            host_seconds=host_seconds, host=host)

    try:
        watcher.start()
        state['head'] = call(['git', 'rev-parse', 'HEAD'], 'git-head', 5)[0].strip()
        state['runtime_sha256'] = isolated.digest(runtime)
        if any(case.endswith('-proof') for case in cases):
            for version in versions:
                label = f'optimized-capability-{version}'
                state['setup'].append(consume(version, 'chain-16', False, label, out / f'{label}.wasm'))
        coverage = {}
        for case in cases:
            for version in versions:
                label = f'coverage-{version}-{case}'
                coverage[version, case] = consume(version, case, False, label, out / f'{label}.wasm', False)
                state['setup'].append(coverage[version, case])
                label = f'warmup-{version}-{case}'
                state['setup'].append(consume(version, case, False, label, out / f'{label}.wasm'))
        jobs = [(case, sample, phase) for case in cases for sample in range(args.samples)
                for phase in phases]
        order = random.Random(0)
        order.shuffle(jobs)
        for case, sample, phase in jobs:
            selected = list(versions)
            order.shuffle(selected)
            for version in selected:
                label = f'{version}-{case}-{sample}-{phase}'
                output = out / f'{label}.wasm'
                base = coverage[version, case]
                if phase != 'fresh':
                    setup = consume(version, case, False, label + '-setup', output)
                    if case.endswith('-proof'):
                        assert setup['claim_fingerprint'] == base['claim_fingerprint'] and setup['wasm_sha256'] == base['wasm_sha256']
                    state['setup'].append(setup)
                existed = output.exists()
                previous = isolated.digest(output) if existed else None
                row = consume(version, case, phase == 'edit', label, output)
                assert existed == (phase != 'fresh')
                if phase == 'unchanged':
                    assert row['wasm_sha256'] == previous
                if case.endswith('-proof'):
                    assert row['wasm_sha256'] == base['wasm_sha256'] and row['expected_output'] == base['expected_output']
                    assert (row['claim_fingerprint'] != base['claim_fingerprint']) == (phase == 'edit'), 'claim edit not consumed'
                    assert (row['source_sha256'] != base['source_sha256']) == (phase == 'edit')
                elif phase == 'edit':
                    assert row['wasm_sha256'] != previous and row['expected_output'] != base['expected_output'], 'code edit not consumed'
                state['rows'].append(dict(case=case, sample=sample, phase=phase, compiler_label=version,
                                          output_existed=existed, **row))
                save()
        state['summary'] = []
        for version in versions:
            for case in cases:
                for phase in phases:
                    rows = [row for row in state['rows'] if (row['compiler_label'], row['case'], row['phase']) == (version, case, phase)]
                    state['summary'].append(dict(compiler_label=version, case=case, phase=phase,
                        compiler_seconds=distribution([row['compiler_seconds'] for row in rows]),
                        host_seconds=distribution([row['host_seconds'] for row in rows]),
                        phase_ns={key: distribution([row['phase_ns'][key] for row in rows]) for key in rows[0]['phase_ns']}))
        if args.baseline:
            lookup = {(row['case'], row['sample'], row['phase'], row['compiler_label']): row for row in state['rows']}
            pairs = []
            for case in cases:
                for phase in phases:
                    differences, ratios = [], []
                    for sample in range(args.samples):
                        old, new = (lookup[case, sample, phase, version] for version in ['baseline', 'candidate'])
                        assert old['source_sha256'] == new['source_sha256'] and old['wasm_sha256'] == new['wasm_sha256']
                        assert old['claim_fingerprint'] == new['claim_fingerprint']
                        differences.append(new['compiler_seconds'] - old['compiler_seconds'])
                        ratios.append(new['compiler_seconds'] / old['compiler_seconds'])
                    pairs.append(dict(case=case, phase=phase, candidate_minus_baseline_seconds=distribution(differences),
                                      candidate_over_baseline=distribution(ratios)))
            state['paired'] = pairs
        assert not isolated.CANCEL.is_set()
        state['status'] = 'passed'
    except BaseException as error:
        state.update(status='failed', error=repr(error))
        raise
    finally:
        finished.set()
        watcher.join()
        state['cleanup_errors'] = []
        if isolated.PROCESSES:
            isolated.cancel_commands()
            for process in list(isolated.PROCESSES):
                try:
                    isolated.reap_command_group(process)
                    with isolated.PROCESS_LOCK:
                        isolated.PROCESSES.discard(process)
                except BaseException as error:
                    state['cleanup_errors'].append(repr(error))
        state.update(source_unchanged=isolated.snapshot(root, isolated.tracked(root)) == pins,
            compiler_unchanged=all(isolated.digest(path) == compiler_pins[key] for key, path in versions.items()),
            fixture_unchanged=fixture is None or isolated.digest(fixture) == fixture_pin,
            all_children_reaped=not isolated.PROCESSES)
        okay = all(state[key] for key in ['source_unchanged', 'compiler_unchanged', 'fixture_unchanged', 'all_children_reaped'])
        if not okay:
            state.update(status='failed', terminal_pin_or_cleanup_failure=True)
        save()
        assert okay
    print(json.dumps(state['summary'], indent=2))


if __name__ == '__main__':
    main()
