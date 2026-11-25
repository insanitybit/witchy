#!/usr/bin/env python3
"""Paired native measurements of two compilers (or one compiler in two modes).

    scripts/native-bench-pair.py --baseline BIN --candidate BIN --binding READY
        [--baseline-env NAME=VALUE ...] [--candidate-env NAME=VALUE ...]
        [--proof-cache DIR] [--pairs 30] [--slot FILE] [--json FILE]

Builds words, str, map and vecloop (benchmarks/witchy) and the TOML parser (benchmarks/toml/bench.wy,
on devsm.toml and zed-lock.toml) for macOS ARM64 with each compiler, then runs them in pairs:
baseline then candidate, or the reverse on every other pair, so drift in the machine falls on
both. Every run must exit 0 and print what the first baseline run printed; a run that does not
stops the measurement. Retired instructions and cycles come from `/usr/bin/time -l`.

For each program the table gives the baseline mean, the mean of the paired differences
(candidate - baseline) as a percentage of it, and that difference's 95% interval (Student's t
over the pairs). An interval that does not hold 0 is a change the measurement resolves.

The benchmark slot: the script holds an exclusive lock on --slot (default
/tmp/witchy-benchmark-slot.lock, the same file for every checkout on the machine, made if
missing) for the whole measurement and waits for it if another measurement holds it, so two do
not time each other. The header pins
what was measured: both compilers' digests, the checkout's commit, the target and the inputs.
"""
import argparse, fcntl, hashlib, json, math, os, re, shutil, statistics, subprocess, sys, tempfile, time
from pathlib import Path

# The machine's builds run through MBX (AGENTS.md), so the slot is an MBX reservation of every
# core: cooperating builds wait while the pairs run. The lock file only orders two of these runs.
_mbx = Path(__file__).resolve().parents[2] / 'state/mbx/bin/mbx'
if not _mbx.exists():  # a linked worktree: `state` is beside the main checkout's files
    _common = subprocess.run(['git', '-C', str(Path(__file__).resolve().parent), 'rev-parse', '--path-format=absolute', '--git-common-dir'], capture_output=True, text=True).stdout.strip()
    _mbx = Path(_common).parent / 'state/mbx/bin/mbx'
_want = f'reserve --cpus {os.cpu_count()} '
# Held or not is read from the parent process, never from the environment, which is inherited.
_parent = subprocess.run(['ps', '-o', 'command=', '-p', str(os.getppid())], capture_output=True, text=True).stdout
if not (_parent.split(' ')[0].endswith('/mbx') and _want in _parent):
    # Inside another reservation (MBX_SCHED_DISABLE) asking for every core would wait on itself.
    if not _mbx.exists() or 'MBX_SCHED_DISABLE' in os.environ or os.environ.get('WITCHY_BENCH_RESERVING') == str(os.getppid()):
        sys.exit(f'the benchmark slot is not held (MBX at {_mbx}; parent: {_parent.strip()[:80]!r})')
    _reserve = [str(_mbx), 'reserve', '--cpus', str(os.cpu_count()), '--', sys.executable, *sys.argv]
    # mbx becomes this process, so the measuring child's parent has this pid: a second failure stops.
    os.execve(_mbx, _reserve, {**os.environ, 'WITCHY_BENCH_RESERVING': str(os.getpid())})

p = argparse.ArgumentParser()
p.add_argument('--baseline', type=Path, required=True)
p.add_argument('--candidate', type=Path, required=True)
p.add_argument('--baseline-env', nargs='*', default=[])
p.add_argument('--candidate-env', nargs='*', default=[])
p.add_argument('--binding', type=Path, required=True)
p.add_argument('--proof-cache', type=Path)
p.add_argument('--pairs', type=int, default=30)
p.add_argument('--slot', type=Path)
p.add_argument('--json', type=Path)
p.add_argument('--toml-counts', default='devsm=20000,zed-lock=200', help='parses of each TOML input in one run')
a = p.parse_args()
repo = Path(__file__).resolve().parents[1]
base = {k: v for k, v in os.environ.items() if not k.startswith(('WITCHY_', 'LEAN_', 'LAKE_'))}
base['WITCHY_WASM_READY_BINDING'] = str(a.binding.resolve())
if a.proof_cache:
    base['WITCHY_PROOF_CACHE'] = str(a.proof_cache.resolve())
SIDES = {'baseline': (a.baseline.resolve(), dict(e.split('=', 1) for e in a.baseline_env)),
         'candidate': (a.candidate.resolve(), dict(e.split('=', 1) for e in a.candidate_env))}
# Student's t, two-sided 95%, by degrees of freedom (the pairs less one).
T95 = {4: 2.776, 9: 2.262, 14: 2.145, 19: 2.093, 24: 2.064, 29: 2.045, 39: 2.023, 49: 2.010, 59: 2.001}

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()[:16]

def measure(command):
    ran = subprocess.run(['/usr/bin/time', '-l', *command], capture_output=True, env=base)
    report = ran.stderr.decode(errors='replace')
    found = [re.search(rf'(\d+)\s+{what}', report) for what in ('instructions retired', 'cycles elapsed')]
    if ran.returncode != 0 or not all(found):
        sys.exit(f'{command}: exit {ran.returncode}\n{report}')
    return int(found[0].group(1)), int(found[1].group(1)), ran.stdout

slot = a.slot or Path('/tmp/witchy-benchmark-slot.lock')
with open(slot, 'a+') as lock, tempfile.TemporaryDirectory(prefix='witchy-bench-') as work:
    waited = time.monotonic()
    fcntl.flock(lock, fcntl.LOCK_EX)
    lock.seek(0); lock.truncate(); lock.write(f'pid {os.getpid()} native-bench-pair since {time.strftime("%Y-%m-%d %H:%M:%S")}\n'); lock.flush()
    work = Path(work).resolve()  # a grant names a directory by its real path
    programs = {name: (repo / 'benchmarks/witchy' / f'{name}.wy', []) for name in ('words', 'str', 'map', 'vecloop')}
    for pair in a.toml_counts.split(','):
        name, count = pair.split('=')
        folder = work / f'toml-{name}'
        folder.mkdir()
        shutil.copy(repo / 'benchmarks/toml/data' / f'{name}.toml', folder / 'input.toml')
        (folder / 'count').write_text(count)
        programs[f'toml-{name}'] = (repo / 'benchmarks/toml/bench.wy', [f'--dir={folder}'])
    built = {}
    for side, (compiler, extra) in SIDES.items():
        for source in sorted({source for source, _ in programs.values()}):
            exe = work / f'{source.stem}.{side}'
            made = subprocess.run([compiler, 'build', source, '-o', exe, '--target=macos-arm64'], env={**base, **extra}, capture_output=True, text=True)
            if made.returncode != 0:
                sys.exit(f'{side}: {source}: {made.stderr}')
            built[side, source] = exe
    commit = subprocess.run(['git', '-C', repo, 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True).stdout.strip()
    dirty = subprocess.run(['git', '-C', repo, 'status', '--porcelain', '--untracked-files=no'], capture_output=True, text=True).stdout.strip()
    header = {'checkout': commit + ('+changes' if dirty else ''), 'target': 'macos-arm64', 'pairs': a.pairs,
              'slot': str(slot), 'waited_for_slot_s': round(time.monotonic() - waited, 1), 'load_before': os.getloadavg()[0],
              **{side: {'compiler': str(compiler), 'sha256': digest(compiler), 'env': extra} for side, (compiler, extra) in SIDES.items()},
              'toml_counts': a.toml_counts}
    print(json.dumps(header), flush=True)
    print('| program | baseline instructions | change | 95% interval | baseline cycles | change | 95% interval |')
    print('|---|---|---|---|---|---|---|', flush=True)
    results = {}
    t = T95.get(a.pairs - 1) or (1.96 + 2.4 / max(a.pairs - 1, 1))
    for name, (source, arguments) in programs.items():
        expected, samples = None, {'baseline': [], 'candidate': []}
        for pair in range(a.pairs):
            for side in (('baseline', 'candidate') if pair % 2 == 0 else ('candidate', 'baseline')):
                instructions, cycles, printed = measure([built[side, source], '--output', '--allocator', *arguments])
                expected = printed if expected is None else expected
                if printed != expected or not printed:
                    sys.exit(f'{name}: the {side} run printed {printed[:200]!r}, not {expected[:200]!r}')
                samples[side].append((instructions, cycles))
        row, cells = {'output_sha256': hashlib.sha256(expected).hexdigest()[:16]}, []
        for column, what in enumerate(('instructions', 'cycles')):
            before = [s[column] for s in samples['baseline']]
            differences = [c[column] - b[column] for b, c in zip(samples['baseline'], samples['candidate'])]
            mean, change = statistics.mean(before), statistics.mean(differences)
            half = t * statistics.stdev(differences) / math.sqrt(len(differences)) if len(differences) > 1 else float('nan')
            row[what] = {'baseline_mean': mean, 'difference_mean': change, 'difference_half_interval': half}
            cells += [f'{mean / 1e6:,.1f} M', f'{100 * change / mean:+.2f}%', f'{100 * (change - half) / mean:+.2f}% to {100 * (change + half) / mean:+.2f}%']
        results[name] = row
        print(f'| {name} | ' + ' | '.join(cells) + ' |', flush=True)
    header['load_after'] = os.getloadavg()[0]
    if a.json:
        a.json.write_text(json.dumps({'header': header, 'programs': results}, indent=1))
    lock.seek(0); lock.truncate()
