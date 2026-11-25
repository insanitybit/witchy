#!/usr/bin/env python3
"""Fast focused check: compile each FIXTURE.wy (imports resolved by the compiler),
run it in Wasmtime and compare stdout with FIXTURE.expected.

A fixture named *.refused.wy must instead fail to compile, with every line of
FIXTURE.expected appearing in the compiler's diagnostics.

  scripts/run-fixtures.py [--compiler BIN] [--runtime HOST] [--binding FILE] [--proof-cache DIR] [--jobs N] FIXTURE.wy...

Programs with proof obligations need --binding (a ready helper binding);
proof-free programs need no Lean. Each obligation must be freshly kernel checked,
unless --proof-cache names a directory of proofs this binding already accepted:
then an obligation may also be a cache hit, and the line says how many were.

An optional FIXTURE.policy is checked with `wasm-caps --policy` from the repository root.
An optional FIXTURE.flags can set `compile-timeout:` in seconds for long fresh
proof runs (the default is 120). It holds `build:` and `run:` lines with extra arguments for
the compiler (memory sizes) and the host (fuel and memory grants). For fixed secrets it may
also hold `env:` lines (`NAME=VALUE ...`, variables set for the host only), one `fd:` line
(hex bytes the host can read from a pipe whose number replaces `{fd}` in the run flags), an
`fd-held: yes` line (that pipe's writer stays open for the whole run: it never ends), a `leak:`
line (hex bytes in a pipe the host is started with and that no flag names, which no program the
module starts may inherit), and `absent:` lines (text that must appear nowhere in the host's
output or errors: a canary). For a secret that is a file: `scratch-file: NAME HEX` writes those
bytes to NAME in `{scratch}` (NAME may be in a directory there, made first), `scratch-link: NAME
LINK` hard-links LINK to it there, `scratch-symlink: NAME LINK` makes LINK a symbolic link to
it (both in `{scratch}`, LINK's directory made first), and each `fd-file: PATH` line opens PATH to read, its
descriptor replacing `{fd-file0}`, `{fd-file1}`, ... in the run flags in the order the lines
come. `{identity:NAME}` in a later line is that scratch file's `DEVICE:INODE`, as
`--secret-object` takes it.

When the fixtures given include accepted ones in tests/fixtures/vacant-assign or
tests/fixtures/owning-helpers and a binding is given, scripts/check-guarded-stores.py then
builds each of those with and without WITCHY_AUDIT_GUARDED_STORES=1 and requires the same
behaviour of both (with all of them given, the four-node list client too); a difference fails
the run. --no-guarded-stores leaves that out.
"""
import re
import argparse, concurrent.futures, json, os, subprocess, sys, tempfile, time
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--compiler', type=Path, default=os.environ.get('WITCHYC', 'witchyc'))
p.add_argument('--runtime', type=Path, default=os.environ.get('WITCHY_WASMTIME_HOST', 'witchy-wasmtime-host'))
p.add_argument('--binding', type=Path)
p.add_argument('--proof-cache', type=Path)
p.add_argument('--jobs', type=int, default=1, help='fixtures checked at once (default 1)')
p.add_argument('--no-guarded-stores', action='store_true', help='do not compare the store and guarded forms of owner assignments')
p.add_argument('fixtures', nargs='+', type=Path)
a = p.parse_args()
repo = Path(__file__).resolve().parents[1]
env = {k: v for k, v in os.environ.items() if not k.startswith(('WITCHY_', 'LEAN_', 'LAKE_'))}
env['WITCHY_WASMTIME_HOST'] = str(a.runtime)
if a.binding:
    env['WITCHY_WASM_READY_BINDING'] = str(a.binding)
accepted = {'fresh_kernel_checked'}
if a.proof_cache:
    a.proof_cache.mkdir(parents=True, exist_ok=True)
    env['WITCHY_PROOF_CACHE'] = str(a.proof_cache.resolve())
    accepted.add('cached_kernel_checked')
def check(index, fixture, scratch):
    expected = fixture.with_suffix('.expected').read_text()
    module = Path(scratch) / f'{index}.wasm'
    flags = {'build': [], 'run': [], 'compile-timeout': [], 'env': [], 'fd': [], 'fd-held': [], 'leak': [], 'absent': [],
             'scratch-file': [], 'scratch-link': [], 'scratch-symlink': [], 'fd-file': []}
    if fixture.with_suffix('.flags').exists():
        for line in fixture.with_suffix('.flags').read_text().splitlines():
            kind, _, rest = line.partition(':')
            # `{dir}` is the fixture's own directory: an input file sits beside its program.
            # `{scratch}` is an empty directory of its own, for a program that writes files.
            own = Path(scratch) / f'{index}.files'
            if '{scratch}' in rest or kind.strip().startswith('scratch-'): own.mkdir(exist_ok=True)
            # The device as the host widens it (a negative macOS dev_t sign-extends).
            identity = lambda m: (lambda st: f'{st.st_dev & (2**64 - 1)}:{st.st_ino}')(os.stat(own / m.group(1)))
            rest = re.sub(r'\{identity:([^}]+)\}', identity, rest)
            words = rest.replace('{dir}', str(fixture.parent.resolve())).replace('{scratch}', str(own)).split()
            if kind.strip() == 'scratch-file':
                (own / words[0]).parent.mkdir(parents=True, exist_ok=True)
                (own / words[0]).write_bytes(bytes.fromhex(''.join(words[1:])))
            elif kind.strip() == 'scratch-link': os.link(own / words[0], own / words[1])
            elif kind.strip() == 'scratch-symlink':
                (own / words[1]).parent.mkdir(parents=True, exist_ok=True)
                os.symlink(own / words[0], own / words[1])
            else: flags[kind.strip()] += words
    timeout = 120
    if flags['compile-timeout']:
        seconds, = flags['compile-timeout']
        timeout = int(seconds)
        if timeout <= 0:
            raise ValueError(f'{fixture}: compile-timeout must be positive')
    started = time.monotonic()
    built = subprocess.run([a.compiler, 'wasm-checked', fixture, '-o', module, *flags['build']],
                           env=env, capture_output=True, text=True, timeout=timeout)
    seconds = time.monotonic() - started
    if fixture.name.endswith('.refused.wy'):
        # Only the messages count: the report also shows the path and the source line, and an
        # expected text found there would pass a fixture refused for some other reason.
        # A failed proof is reported in lines of its own, which no source line can start like.
        # (`W0401: message`; a compiler from before 2026-11-22 wrote `W0401 12..34: message`.)
        messages = '\n'.join(re.findall(r'W\d{4}(?: \d+\.\.\d+| \([^)]*\))?: (.*)', built.stderr)
                             + [line for line in built.stderr.splitlines() if line.startswith(('proof failed: ', '  not proved: '))])
        missing = [line for line in expected.splitlines() if line not in messages]
        ok = built.returncode != 0 and not module.exists() and not missing
        detail = 'refused' if ok else f'exit {built.returncode}, missing {missing}\n{built.stderr}'
    elif built.returncode != 0:
        ok, detail = False, built.stderr
    else:
        goals = json.loads(built.stdout)['obligations']
        stale = [g for g in goals if g['outcome'] not in accepted]
        hits = sum(g['outcome'] == 'cached_kernel_checked' for g in goals)
        # The allocator is granted only to a program whose `main` asks for one, so a program that
        # never holds one is checked to run without it (a library naming `Allocator` in a
        # function the program never reaches once made every console program import it).
        asks = re.search(r'fn main\([^)]*Allocator', fixture.read_text())
        started = time.time()
        # A secret on a descriptor: a pipe holding the bytes, its write end closed, inherited
        # by this run alone (Python's descriptors are not inherited unless passed).
        reading = held = leaked = None
        if flags['fd']:
            reading, writing = os.pipe()
            os.write(writing, bytes.fromhex(''.join(flags['fd'])))
            if flags['fd-held'] == ['yes']: held = writing
            else: os.close(writing)
        if flags['leak']:
            leaked, writing = os.pipe()
            os.write(writing, bytes.fromhex(''.join(flags['leak'])))
            os.close(writing)
        files = [os.open(path, os.O_RDONLY) for path in flags['fd-file']]
        run_flags = [f.replace('{fd}', str(reading)) for f in flags['run']]
        for number, fd in enumerate(files):
            run_flags = [f.replace(f'{{fd-file{number}}}', str(fd)) for f in run_flags]
        run_env = {**env, **dict(pair.split('=', 1) for pair in flags['env'])}
        try:
            ran = subprocess.run([a.runtime, module, '--output', *(['--allocator'] if asks else []), *run_flags],
                                 env=run_env, capture_output=True, text=True, timeout=30,
                                 pass_fds=tuple(fd for fd in (reading, leaked, *files) if fd is not None))
        finally:
            for fd in (reading, held, leaked, *files):
                if fd is not None: os.close(fd)
        # The run's wall time: a run near the host's default one-second deadline fails under load.
        running = time.time() - started
        ok = ran.returncode == 0 and ran.stdout == expected and not stale
        # FIXTURE.policy, when there is one, holds each file to what it may use (paths as from the
        # repository root, where the check runs): a package reaching further fails the fixture.
        policy = fixture.with_suffix('.policy')
        if ok and policy.exists():
            checked = subprocess.run([a.compiler, 'wasm-caps', '--policy', policy.resolve(), fixture.resolve().relative_to(repo)],
                                     cwd=repo, env=env, capture_output=True, text=True, timeout=120)
            if checked.returncode != 0:
                ok, stale = False, [f'policy: {checked.stdout}{checked.stderr}']
        if fixture.name.endswith('.traps.wy'):
            # The program is accepted and must stop with a trap: nothing printed, each expected line in the report.
            ok = ran.returncode != 0 and ran.stdout == '' and not stale and all(line in ran.stderr for line in expected.splitlines())
        # A canary: a secret's value, which no output or error may repeat.
        if any(text in ran.stdout + ran.stderr for text in flags['absent']):
            ok, stale = False, [*stale, 'a canary value appeared in the output']
        detail = ('trapped' if ok and fixture.name.endswith('.traps.wy') else f'{len(goals)} obligations{f" ({hits} cached)" if hits else ""}, {len(expected.splitlines())} lines' if ok else
                  f'exit {ran.returncode} stale {len(stale)}\n--- expected\n{expected}--- actual\n{ran.stdout}{ran.stderr}')
    ran_for = f", {running:.2f}s run" if 'running' in locals() else ''
    return ok, f"{'ok  ' if ok else 'FAIL'} {fixture} ({seconds:.1f}s compile{ran_for}) {detail}"

failed = 0
with tempfile.TemporaryDirectory(prefix='witchy-fixtures-') as scratch:
    # Fixtures are independent: each has its own module and scratch directory, and the proof
    # cache takes concurrent compilers (each entry is whole lines in one append). Results print
    # in the order given whatever order they finish in.
    with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as pool:
        for ok, line in pool.map(lambda pair: check(pair[0], pair[1], scratch), enumerate(a.fixtures)):
            failed += not ok
            print(line, flush=True)
# The store an owner assignment becomes against the guarded form its proof was about.
GUARDED = ('vacant-assign', 'owning-helpers')
def assigns(fixture): return fixture.parent.parent == repo / 'tests/fixtures' and fixture.parent.name in GUARDED and not fixture.name.endswith('.refused.wy')
chosen = sorted({f.resolve() for f in a.fixtures if assigns(f.resolve())})
if chosen and a.binding and not a.no_guarded_stores:
    every = sorted(f.resolve() for d in GUARDED for f in (repo / 'tests/fixtures' / d).glob('*.wy') if assigns(f.resolve()))
    print(f'store against guarded form, {len(chosen)} fixtures{" and the list client" if chosen == every else ""}:', flush=True)
    compared = subprocess.run([sys.executable, repo / 'scripts/check-guarded-stores.py', '--compiler', a.compiler, '--runtime', a.runtime, '--binding', a.binding,
                               *(['--proof-cache', a.proof_cache] if a.proof_cache else []), *([] if chosen == every else chosen)], env=env)
    failed += compared.returncode != 0
sys.exit(1 if failed else 0)
