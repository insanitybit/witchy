#!/usr/bin/env python3
"""Focused real Lean/publishing checks. Requires an explicitly admitted host and ready binding."""
import argparse, hashlib, json, os, re, selectors, shutil, signal, subprocess, time
from pathlib import Path
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--compiler', type=Path, required=True)
p.add_argument('--binding', type=Path, required=True)
p.add_argument('--out', type=Path, required=True)
p.add_argument('--runtime', type=Path, required=True)
p.add_argument('--launch-free-gib', type=int, default=16)
a = p.parse_args()
# The outer live-floor supervisor sends SIGTERM; unwind owned-child cleanup.
signal.signal(signal.SIGTERM, signal.default_int_handler)
if a.launch_free_gib < 10: p.error('launch floor must be at least 10 GiB')
repo = Path(__file__).resolve().parents[1]
a.out.mkdir()  # Fresh evidence only.
fields = dict(line.split('=', 1) for line in a.binding.read_text().splitlines()[1:])
root = fields['sdk']
base = dict(PATH=os.environ.get('PATH', '/usr/bin:/bin'), WITCHY_WASMTIME_HOST=str(a.runtime.resolve()))
ready = dict(base, WITCHY_WASM_READY_BINDING=str(a.binding))
source, output = a.out / 'program.wy', a.out / 'program.wasm'
vec = (repo / 'tests/fixtures/ready-vec.wy').read_text()
lemma = 'lemma context_current(x: U64):\n    ensures x + 1 > x\n    proof:\n        auto\n'
report = {'status': 'running', 'commands': [], 'sessions': []}
active = []
def sha(data): return hashlib.sha256(data).hexdigest()
def save():
    tmp = a.out / 'results.pending'; tmp.write_text(json.dumps(report, indent=2) + '\n'); tmp.replace(a.out / 'results.json')
def census():
    rows = subprocess.check_output(['/bin/ps', '-axo', 'pid=,ppid=,pgid=,lstart='], text=True)
    return {int(v[0]): (int(v[1]), int(v[2]), v[3]) for line in rows.splitlines() if len(v := line.split(None, 3)) == 4}
class Process:
    def __init__(self, args, env, name):
        if shutil.disk_usage(a.out).free < a.launch_free_gib * 2**30: raise RuntimeError(f'{a.launch_free_gib} GiB launch floor')
        self.name = name; self.helpers = {}; self.pending = bytearray(); self.closed = False
        self.err = (a.out / (name + '.stderr')).open('wb')
        self.select = selectors.DefaultSelector()
        self.child = subprocess.Popen(list(map(str,args)), env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.err, start_new_session=True)
        active.append(self)  # Custody is registered before fallible pipe setup.
        self.select.register(self.child.stdout, selectors.EVENT_READ)
        os.set_blocking(self.child.stdout.fileno(), False)
    def observe(self):
        if shutil.disk_usage(a.out).free < 4 * 2**30: raise RuntimeError('4 GiB live floor')
        rows = census()
        for _ in range(4):
            parents = {self.child.pid, *self.helpers}
            for pid, row in rows.items():
                if row[0] in parents: self.helpers.setdefault(pid, row)
    def read(self, line=False, timeout=40):
        start = time.monotonic()
        while True:
            self.observe()
            if line and b'\n' in self.pending:
                head, _, self.pending = self.pending.partition(b'\n'); return bytes(head)
            if time.monotonic() - start > timeout: raise TimeoutError(self.name)
            for key, _ in self.select.select(.02):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    if line: raise RuntimeError('unexpected EOF: ' + self.name)
                    data = bytes(self.pending); self.pending.clear(); return data
                self.pending.extend(chunk)
                if len(self.pending) > 262144: raise RuntimeError('test output bound')
    def close(self, expected=0):
        if self.closed: return
        if self.child.stdin and not self.child.stdin.closed: self.child.stdin.close()
        self.child.wait(timeout=15); self.err.close(); self.select.close()
        current = census(); started = time.monotonic()
        while any(pid in current for pid in self.helpers) and time.monotonic()-started < 1:
            time.sleep(.01); current = census()
        state = dict(name=self.name, pid=self.child.pid, exit=self.child.returncode,
            helpers=list(self.helpers), helpers_absent=all(pid not in current for pid in self.helpers))
        report['sessions'].append(state); save()
        assert self.child.returncode == expected and state['helpers_absent'], state
        self.closed = True
    def cleanup(self):
        if self.closed: return
        self.observe()
        # Signal only our direct unreaped Child's group. Artifact custodians
        # release their own groups on parent disconnect; never signal a
        # descendant's numeric PGID from a stale census after its parent reaps.
        if self.child.poll() is None:
            os.killpg(self.child.pid, signal.SIGKILL)
        self.child.wait(timeout=5); self.err.close(); self.select.close(); self.closed=True
        until = time.monotonic() + 3
        while any(pid in census() for pid in self.helpers) and time.monotonic() < until: time.sleep(.01)
        assert all(pid not in census() for pid in self.helpers), 'owned helper remains'
def command(args, env, name, expected=0):
    started = time.monotonic(); proc = Process(args, env, name); proc.child.stdin.close()
    data = proc.read(); proc.close(expected)
    (a.out / (name + '.stdout')).write_bytes(data)
    report['commands'].append(dict(name=name, seconds=time.monotonic()-started, exit=proc.child.returncode)); save()
    return data

def compile(env, name, expected=0):
    return command([a.compiler, 'wasm-checked', source, '-o', output], env, name, expected)
def request(proc, text, name):
    source.write_text(text); started=time.monotonic()
    proc.child.stdin.write(f'build {name} {sha(source.read_bytes())}\n'.encode()); proc.child.stdin.flush()
    record=json.loads(proc.read(True)); assert record['request_id']==name
    assert record['check']['source_sha256']==sha(source.read_bytes())
    assert record['check']['wasm_sha256']==sha(output.read_bytes())
    (a.out/(name+'.json')).write_text(json.dumps(record,indent=2)+'\n')
    report['commands'].append(dict(name=name,seconds=time.monotonic()-started));save()
    return record
try:
    save()
    source.write_text('fn main():\n    return\n')
    plain=json.loads(compile(base,'proof-free'))
    assert plain['obligations']==[] and 'selected_generation_kernel_checks' not in plain
    source.write_text(vec)
    fresh=json.loads(compile(dict(base,WITCHY_WASM_LEAN_ROOT=root),'fresh-fallback'))
    assert fresh['fresh_kernel_checks']['admission_reuse'] is False
    reference=output.read_bytes()
    ordinary=command([a.compiler,'build',source,'-o',output],ready,'ordinary-ready')
    assert not ordinary and output.read_bytes()==reference
    runtime=command([a.runtime.resolve(),output,'--output','--allocator'],base,'runtime')
    expected=('1\n1\n0\n0\n0\n0\n'+'0\n1\n1\n1\n1\n0\n'+'0\n0\n2\n2\n1\n1\n')*4
    assert runtime.decode()==expected
    proc=Process([a.compiler,'build-session',source,'-o',output],ready,'warm-session')
    first=request(proc,vec,'first')
    proc.observe()
    helper_pids=[pid for pid,row in proc.helpers.items() if row[0]==proc.child.pid]
    assert len(helper_pids)==1
    mapped=subprocess.check_output(['/usr/sbin/lsof','-Fn','-p',str(helper_pids[0])],text=True)
    images=[line[1:] for line in mapped.splitlines() if line.startswith('n') and '/lib/lean/' in line and line.endswith('.dylib')]
    assert images and all(Path(image).is_relative_to(Path(root)) for image in images)
    report['loaded_sdk_images']=images;save()
    second=request(proc,lemma+vec,'valid-edit')
    for record, count in [(first,4),(second,5)]:
        proof=record['check']['selected_generation_kernel_checks']
        assert len(record['check']['obligations'])==count and proof['admission_reuse'] is True
        assert proof['verified_immutable'] is False and proof['sdk_path_currentness'] is False
        assert 'body_install_ns' not in proof and output.read_bytes()==reference
    assert second['measurements']['phases']['fresh_proof_check']['counts']['installation_snapshots']==0
    source.write_text(lemma.replace('x + 1 > x','x + 1 < x')+vec)
    proc.child.stdin.write(f'build false-unused {sha(source.read_bytes())}\n'.encode());proc.child.stdin.flush()
    assert not proc.read();proc.close(1);assert output.read_bytes()==reference
    assert 'W0402' in (a.out/'warm-session.stderr').read_text()
    evidence = Path(re.findall(r'native diagnostic raw evidence: (.+)', (a.out/'warm-session.stderr').read_text())[-1])
    failed_request = json.loads((evidence/'request-4.json').read_text())
    raw = (evidence/'stdout').read_text()
    frames = [json.loads(line.split(':',1)[1]) for line in raw.splitlines() if line.startswith('WITCHY_RESIDENT_V2:')]
    assert failed_request['source'] == sha(source.read_bytes()) and failed_request['body'] != '0'*64
    assert any(frame['seq']==4 and frame['status']=='error' for frame in frames)
    assert not any(frame['seq']==4 and frame['status']=='ok' for frame in frames)
    assert ': error:' in raw and ('omega' in raw or 'unsolved goals' in raw or 'simp_all made no progress' in raw)
    report['false_unused'] = dict(evidence=str(evidence), actual_lean_error=True, current_source=failed_request['source']);save()

    # Wrong generation and wrong helper hash are refusals, never fresh fallback.
    source.write_text(vec)
    for key,name in [('sdk_sha256','wrong-generation'),('helper_sha256','wrong-helper')]:
        record=a.binding.read_text().replace(key+'='+fields[key],key+'='+'0'*64)
        config=a.out/(name+'.txt');config.write_text(record)
        assert not compile(dict(base,WITCHY_WASM_READY_BINDING=str(config)),name,1)
        assert output.read_bytes()==reference
        assert 'native diagnostic raw evidence:' not in (a.out/(name+'.stderr')).read_text()
    # Changing source while its real checker is running cannot publish the old snapshot.
    output.write_bytes(b'preserved old output');source.write_text(vec)
    proc=Process([a.compiler,'build',source,'-o',output],ready,'source-race');proc.child.stdin.close()
    started=time.monotonic()
    while not proc.helpers:
        proc.observe()
        assert proc.child.poll() is None and time.monotonic()-started<20
        time.sleep(.01)
    source.write_text(lemma+vec)
    assert not proc.read();proc.close(1)
    assert output.read_bytes()==b'preserved old output'
    assert 'source changed during checking' in (a.out/'source-race.stderr').read_text()
    # Normal watch shutdown closes the active checker instead of orphaning it.
    source.write_text(vec)
    proc=Process([a.compiler,'build',source,'-o',output,'--watch'],ready,'watch-stop')
    started=time.monotonic()
    while 'built ' not in (a.out/'watch-stop.stderr').read_text():
        proc.observe();assert proc.child.poll() is None and time.monotonic()-started<40;time.sleep(.02)
    proc.child.send_signal(signal.SIGTERM);assert not proc.read();proc.close()
    assert output.read_bytes()==reference
    # Launcher group cancellation must include the otherwise-idle persistent helper.
    proc=Process([a.compiler,'build-session',source,'-o',output],ready,'launcher-kill')
    request(proc,vec,'before-kill');proc.observe()
    assert proc.helpers and all(row[1]==proc.child.pid for row in proc.helpers.values())
    os.killpg(proc.child.pid,signal.SIGKILL)
    assert not proc.read();proc.close(-signal.SIGKILL)
    # Running with claims is a fresh check of the current source: the same bytes, the same output.
    recheck=a.out/'recheck.wasm'
    command([a.compiler,'build',source,'-o',recheck],ready,'recheck')
    assert recheck.read_bytes()==reference
    run=command([a.runtime.resolve(),recheck,'--output','--allocator'],base,'recheck-run')
    assert run.decode()==expected
    report['status']='passed';save()
except BaseException as error:
    report.update(status='failed', error=repr(error)); save(); raise
finally:
    for proc in active: proc.cleanup()
    save()
