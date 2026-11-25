#!/usr/bin/env python3
"""Real proof-bearing edit loops across bounded-helper renewal; exclusive host required."""
import argparse, hashlib, json, os, re, shutil, signal, subprocess, time
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--compiler', type=Path, required=True)
p.add_argument('--binding', type=Path, required=True)
p.add_argument('--runtime', type=Path, required=True)
p.add_argument('--out', type=Path, required=True)
p.add_argument('--case', choices=['session', 'watch', 'binding-change', 'stop-failure'], required=True)
a = p.parse_args()
signal.signal(signal.SIGTERM, signal.default_int_handler)
assert shutil.disk_usage(a.out.parent).free >= 10 * 2**30, '10 GiB launch floor'
a.out.mkdir()
repo = Path(__file__).resolve().parents[1]
binding = a.out / 'ready-binding.txt'
binding.write_bytes(a.binding.read_bytes())
env = dict(PATH=os.environ.get('PATH', '/usr/bin:/bin'),
           WITCHY_WASM_READY_BINDING=str(binding.resolve()), WITCHY_WASMTIME_HOST=str(a.runtime.resolve()))
source, output = a.out / 'program.wy', a.out / 'program.wasm'
vec = (repo / 'tests/fixtures/ready-vec.wy').read_text()
def text(index, false=False):
    proposition = 'x + 1 < x' if false else 'x + 1 > x'
    return f'lemma current_{index}(x: U64):\n    ensures {proposition}\n    proof:\n        auto\n' + vec
def sha(data): return hashlib.sha256(data).hexdigest()
def children():
    rows = subprocess.check_output(['/bin/ps', '-axo', 'pid=,ppid='], text=True)
    return {int(pid): int(parent) for pid, parent in map(str.split, rows.splitlines())}
def absent(pid):
    try: os.kill(pid, 0)
    except ProcessLookupError: return True
    return False  # Includes permission/ownership uncertainty: never inferred released.
report = dict(status='running', case=a.case, edits=[], helpers=[], binding_sha256=sha(binding.read_bytes()))
def save():
    tmp = a.out / 'results.pending'; tmp.write_text(json.dumps(report, indent=2)+'\n'); tmp.replace(a.out / 'results.json')
stdout_path, stderr_path = a.out / 'stdout', a.out / 'stderr'
def observe():
    assert shutil.disk_usage(a.out).free >= 4 * 2**30, '4 GiB live floor'
    assert all(f.stat().st_size < 2**20 for f in [stdout_path, stderr_path]), 'bounded harness output'
    for pid, parent in children().items():
        if parent == child.pid and pid not in report['helpers']: report['helpers'].append(pid)
def wait_for(predicate, seconds=40, require_live=True):
    started = time.monotonic()
    while not predicate():
        observe()
        if require_live: assert child.poll() is None, stderr_path.read_text()
        assert time.monotonic() - started < seconds, 'edit deadline'
        time.sleep(.01)
    observe()
source.write_text(text(0))
out = stdout_path.open('wb'); err = stderr_path.open('wb')
child = subprocess.Popen([str(a.compiler), 'build' if a.case == 'watch' else 'build-session',
                         str(source), '-o', str(output), *(['--watch'] if a.case == 'watch' else [])],
                        env=env, stdin=subprocess.PIPE, stdout=out, stderr=err, start_new_session=True)
save()
try:
    count = 12 if a.case in ['session', 'watch'] else 5
    reference = None
    for index in range(count):
        next_source = text(index)
        pending = a.out / 'next.wy'; pending.write_text(next_source); pending.replace(source)
        before = time.monotonic()
        if a.case != 'watch':
            child.stdin.write(f'build edit_{index} {sha(source.read_bytes())}\n'.encode()); child.stdin.flush()
            wait_for(lambda: len(stdout_path.read_bytes().splitlines()) == index + 1)
            receipt = json.loads(stdout_path.read_text().splitlines()[-1])
            assert receipt['request_id'] == f'edit_{index}'
            assert receipt['check']['source_sha256'] == sha(source.read_bytes())
            assert receipt['check']['wasm_sha256'] == sha(output.read_bytes())
            assert receipt['check']['selected_generation_kernel_checks']['ready_binding_sha256'] == report['binding_sha256']
        else:
            wait_for(lambda: stderr_path.read_text().count('built ') == index + 1)
            receipt = None
        elapsed = time.monotonic() - before
        if reference is None: reference = output.read_bytes()
        assert output.read_bytes() == reference
        live = [pid for pid in report['helpers'] if not absent(pid)]
        assert len(live) == 1, live
        report['edits'].append(dict(index=index, seconds=elapsed, helper=live[0],
                                    source_sha256=sha(source.read_bytes()), receipt=receipt))
        save()
    if count == 12:
        helpers = [report['edits'][i]['helper'] for i in [0, 5, 10]]
        assert len(set(helpers)) == 3, helpers
        assert all(absent(pid) for pid in helpers[:2]), 'retired helpers not reaped'
        assert all(report['edits'][i]['helper'] == helpers[i // 5] for i in range(count))
        if a.case == 'session':
            snapshots = [row['receipt']['measurements']['phases']['fresh_proof_check']['counts']['installation_snapshots'] for row in report['edits']]
            assert snapshots == [2 if i in [0, 5, 10] else 0 for i in range(count)], snapshots
    if a.case == 'binding-change':
        # Change only the test-owned record, never the installed SDK/helper.
        binding.write_text(binding.read_text().replace('helper_mode=body-interface-v2', 'helper_mode=invalid'))
    if a.case == 'stop-failure':
        helper = report['edits'][-1]['helper']
        assert children().get(helper) == child.pid and not absent(helper)
        os.kill(helper, signal.SIGSTOP)  # Actual owned helper cannot acknowledge STOP.
    pending = a.out / 'next.wy'; pending.write_text(text(count, false=a.case in ['session', 'watch'])); pending.replace(source)
    if a.case != 'watch':
        child.stdin.write(f'build refused {sha(source.read_bytes())}\n'.encode()); child.stdin.flush()
    wait_for(lambda: child.poll() is not None, require_live=False)
    assert child.wait(timeout=5) == 1
    assert output.read_bytes() == reference
    assert len(stdout_path.read_bytes().splitlines()) == (0 if a.case == 'watch' else count)
    assert stderr_path.read_text().count('built ') == (count if a.case == 'watch' else 0)
    assert all(absent(pid) for pid in report['helpers']), 'helper remains'
    if a.case in ['binding-change', 'stop-failure']: assert len(report['helpers']) == 1, 'replacement after refusal'
    errors = stderr_path.read_text()
    if a.case == 'binding-change': assert 'selected ready binding/root/helper changed' in errors
    if a.case == 'stop-failure': assert 'native diagnostic deadline' in errors
    if a.case in ['session', 'watch']:
        assert 'W0402' in errors
        evidence = Path(re.findall(r'native diagnostic raw evidence: (.+)', errors)[-1])
        raw = (evidence/'stdout').read_text()
        assert 'simp_all made no progress' in raw, raw
        frames = [json.loads(line.split(':', 1)[1]) for line in raw.splitlines() if line.startswith('WITCHY_RESIDENT_V2:')]
        failed = [frame for frame in frames if frame['status'] == 'error']
        assert len(failed) == 1
        request = json.loads((evidence/f"request-{failed[0]['seq']}.json").read_text())
        assert request['op'] == 'CHECK' and request['source'] == sha(source.read_bytes())
        assert not any(frame['seq'] == failed[0]['seq'] and frame['status'] == 'ok' for frame in frames)
        shutil.copytree(evidence, a.out/'native-refusal-evidence')
        report['actual_current_false_unused_kernel_refusal'] = True
        result = subprocess.run([str(a.runtime.resolve()), str(output), '--output', '--allocator'],
                                env=env, capture_output=True, timeout=5)
        expected = ('1\n1\n0\n0\n0\n0\n'+'0\n1\n1\n1\n1\n0\n'+'0\n0\n2\n2\n1\n1\n')*4
        assert result.returncode == 0 and result.stdout.decode() == expected and not result.stderr
        (a.out/'runtime.stdout').write_bytes(result.stdout)
    report.update(status='passed', old_output_preserved=True, helpers_absent=True)
except BaseException as error:
    report.update(status='failed', error=repr(error)); raise
finally:
    if child.poll() is None: os.killpg(child.pid, signal.SIGKILL)
    child.wait(timeout=5)
    child.stdin.close(); out.close(); err.close()
    report['compiler_reaped'] = True
    report['helpers_absent_after_cleanup'] = all(absent(pid) for pid in report['helpers'])
    save()
    assert report['helpers_absent_after_cleanup'], 'helper ownership unresolved; reservation must remain held'
