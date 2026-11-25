#!/usr/bin/env python3
"""Proof-free, consumed-edit baseline; acquire the coordinator host lease first.

Use an already built optimized compiler and an evidence directory outside the
checkout. Fresh means absent output, not a cold filesystem or cold process cache.
Unchanged and edited inputs are fully rechecked; this harness assumes no cache.
Compiler invocation and disposable host invocation are timed separately. Pass --baseline for a randomized interleaved comparison of exact compiler binaries.
Fresh still does not mean cold filesystem; this does not compare arbitrary backends.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import random
import signal
import shutil
import statistics
import subprocess
import threading


def distribution(values):
    rng = random.Random(0)
    boot = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(2000))
    return dict(n=len(values), median=statistics.median(values), minimum=min(values),
                maximum=max(values), median_bootstrap_95=[boot[50], boot[1949]])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True, help='the Witchy runtime, tools/wasmtime-host')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, help='preserved optimized compiler for matched pairs')
    parser.add_argument('--samples', type=int, default=12)
    parser.add_argument('--sizes', type=int, nargs='+', default=[16, 64, 120])
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    compiler, out = args.compiler.resolve(), args.out.resolve()
    if root == out or root in out.parents:
        parser.error('evidence directory must be outside checkout')
    if args.samples < 3 or any(not 1 <= size <= 120 for size in args.sizes):
        parser.error('need at least three samples and sizes 1..120')
    out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('isolated', root / 'scripts/harness.py')
    isolated = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(isolated)
    pins = isolated.snapshot(root, isolated.tracked(root))
    def interrupted(signum, _frame):
        isolated.CANCEL.set()
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    compiler_pin = isolated.digest(compiler)
    versions = {'candidate': compiler}
    if args.baseline:
        versions['baseline'] = args.baseline.resolve()
    compiler_pins = {name: isolated.digest(path) for name, path in versions.items()}
    env = {k: v for k, v in os.environ.items() if not k.startswith(
        ('WITCHY_', 'LEAN_', 'LAKE_', 'NODE_', 'DYLD_', 'LD_'))}
    runtime = args.runtime.resolve()
    state = dict(status='running', head=subprocess.check_output(
        ['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
        compiler_sha256=compiler_pin, platform=platform.platform(), machine=platform.machine(),
        process_id=os.getpid(), compiler_pins=compiler_pins, compilers={k: str(v) for k, v in versions.items()},
        harness_sha256=isolated.digest(Path(__file__)),
        samples=args.samples, sizes=args.sizes, rows=[], commands=[],
        scope='optimized native compiler invocation; generic-record chains; randomized interleaved pairs' if args.baseline else 'optimized compiler baseline',
        exclusions=['no causal optimization attribution', 'no hardware counters or allocation instrumentation',
                    'no cold filesystem claim', 'no concurrency sweep', 'no proof-bearing workload'])
    finished = threading.Event()

    def save():
        (out / 'results.json').write_text(json.dumps(state, indent=2) + '\n')

    def watch():
        while not finished.wait(.25):
            if isolated.CANCEL.is_set():
                state['interrupted'] = True
                isolated.cancel_commands()
                return
            if shutil.disk_usage(out).free < 4 * 2**30:
                state['disk_floor'] = True
                isolated.cancel_commands()
                return

    watcher = threading.Thread(target=watch)
    watcher.start()

    def call(argv, label, timeout=10):
        assert shutil.disk_usage(out).free >= 16 * 2**30, '16 GiB launch guard'
        result, seconds = isolated.command(argv, root, env, out / (label + '.log'),
                                           timeout=timeout, separate_stderr=True)
        state['commands'].append(dict(label=label, seconds=seconds, exit=result.returncode))
        assert result.returncode == 0 and not state.get('disk_floor'), label
        return seconds

    def read_json(label):
        return json.loads((out / (label + '.log')).read_text().splitlines()[0])


    def source(size, delta):
        text = 'type Box<T>:\n    value: T\n'
        for index in range(size):
            value = 'x' if index == 0 else f'f{index-1}(x)'
            add = delta if index == 0 else 1
            text += f'fn f{index}(x: Box<U64>) -> Box<U64>:\n    return Box({value}.value + {add})\n'
        return text + f'fn main(console: Console<Write>):\n    console.print_int(f{size-1}(Box(0)).value)\n'

    def consume(binary, text, output, label, size, delta):
        input_path = out / f'chain-{size}.wy'
        input_path.write_text(text)
        seconds = call([str(binary), 'wasm-measure', str(input_path), '-o', str(output)], label)
        measured = read_json(label)
        assert measured['debug_assertions'] is False, 'optimized compiler required'
        assert measured['lean_invocations'] == 0, 'unexpected proof work'
        host_seconds = call([str(runtime), str(output), '--output'], label + '-host')
        host = dict(status='ok', output=(out / (label + '-host.log')).read_text())
        assert host['output'] == f'{size - 1 + delta}\n', host
        return dict(source_sha256=hashlib.sha256(text.encode()).hexdigest(),
            wasm_sha256=isolated.digest(output), compiler_seconds=seconds,
            compiler_phases=measured, host_seconds=host_seconds, host=host)

    try:
        # Setup/warmup is recorded separately, never included in the sample rows.
        for version, binary in versions.items():
            consume(binary, source(args.sizes[0], 1), out / f'warmup-{version}.wasm',
                    f'warmup-{version}', args.sizes[0], 1)
        order = random.Random(0)
        for size in args.sizes:
            for sample in range(args.samples):
                for phase in ['fresh', 'unchanged', 'edit']:
                    delta = 2 if phase == 'edit' else 1
                    text = source(size, delta)
                    selected = list(versions)
                    order.shuffle(selected)
                    for version in selected:
                        output = out / (f'fresh-{version}-{size}-{sample}.wasm' if phase == 'fresh'
                                        else f'current-{version}-{size}.wasm')
                        label = f'{version}-{size}-{sample}-{phase}'
                        if phase == 'unchanged':
                            shutil.copyfile(out / f'fresh-{version}-{size}-{sample}.wasm', output)
                        row = consume(versions[version], text, output, label, size, delta)
                        state['rows'].append(dict(size=size, sample=sample, phase=phase,
                                                  compiler_label=version, **row))
            input_path = out / f'chain-{size}.wy'
            input_path.write_text(source(size, 1))
            for version, binary in versions.items():
                label = f'coverage-{version}-{size}'
                call([str(binary), 'wasm-checked', str(input_path), '-o',
                      str(out / f'{label}.wasm')], label)
                receipt = read_json(label)
                assert receipt['all_selected_checked'] and len(receipt['selected_functions']) == size + 1
                assert receipt['obligations'] == [] and len(receipt['generic_record_templates']) == 1
            save()
        state['summary'] = [dict(compiler_label=version, size=size, phase=phase,
            compiler_seconds=distribution([row['compiler_seconds'] for row in state['rows']
                if row['size'] == size and row['phase'] == phase and row['compiler_label'] == version]))
            for version in versions for size in args.sizes for phase in ['fresh', 'unchanged', 'edit']]
        if args.baseline:
            lookup = {(r['size'], r['sample'], r['phase'], r['compiler_label']): r for r in state['rows']}
            state['paired'] = []
            for size in args.sizes:
                for phase in ['fresh', 'unchanged', 'edit']:
                    differences, ratios = [], []
                    for sample in range(args.samples):
                        old = lookup[size, sample, phase, 'baseline']
                        new = lookup[size, sample, phase, 'candidate']
                        assert old['source_sha256'] == new['source_sha256']
                        differences.append(new['compiler_seconds'] - old['compiler_seconds'])
                        ratios.append(new['compiler_seconds'] / old['compiler_seconds'])
                    state['paired'].append(dict(size=size, phase=phase,
                        candidate_minus_baseline_seconds=distribution(differences),
                        candidate_over_baseline=distribution(ratios)))
        assert not isolated.CANCEL.is_set(), 'measurement interrupted'
        state['status'] = 'passed'
    except BaseException as error:
        state.update(status='failed', error=repr(error))
        raise
    finally:
        finished.set()
        watcher.join()
        state.update(source_unchanged=isolated.snapshot(root, isolated.tracked(root)) == pins,
                     compiler_unchanged=all(isolated.digest(path) == compiler_pins[name]
                                            for name, path in versions.items()),
                     all_children_reaped=not isolated.PROCESSES)
        save()
        assert state['source_unchanged'] and state['compiler_unchanged'] and state['all_children_reaped']
    print(json.dumps(state['summary'], indent=2))


if __name__ == '__main__':
    main()
