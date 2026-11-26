#!/usr/bin/env python3
"""Native fixtures as the compiler emits them, against checked mode.

    scripts/native-checked.py --compiler BIN --binding READY [--proof-cache DIR | --private-proof-cache] [--jobs N] [--measure]
                              [--no-control] [--write-list FILE] FIXTURE.wy...

A fixture is a FIXTURE.wy with a FIXTURE.expected beside it (a .wy without one is a file other
fixtures import, and is only counted). Each is built for the native target three times: as
the compiler emits it; in checked mode (WITCHY_NATIVE_CHECKED=1, src/compiler/native/mod.rs:
every test a proof lets the default build leave out is kept, and every cell access is the
runtime's checked call); and observed (WITCHY_NATIVE_OBSERVED=1, native/body/cells.rs: the
accesses to a PrefixBuffer are written in as in the default build, and the runtime judges each
from its own state and names the address it must reach). In all three a PrefixBuffer is
named by slot and generation. All run with WITCHY_HEAP_AUDIT=1.
What each must do is decided by its name and by scripts/native-checked.list, never by the
builds agreeing ("both builds" below is all three):

  FIXTURE.wy            both builds exit 0 and print FIXTURE.expected, with nothing on
                        standard error.
  FIXTURE.traps.wy      both builds are accepted and stop with a trap (SIGILL or SIGTRAP on
                        macOS) having printed nothing. A native trap says nothing of its
                        cause, so this does not tell the intended trap from another.
  FIXTURE.refused.wy    the build is refused, no executable is written, and every line of
                        FIXTURE.expected is in the diagnostics (one build: a refusal comes
                        before any code is emitted).

A line of the list names a fixture the native target does not run as the Wasm host does, the
way it must fail and a text its message must hold; it is checked to fail that way in both
builds, reported apart, and never counted as passed:

  PATH build-refused TEXT   the native build is refused with TEXT in its diagnostics
  PATH launch-refused TEXT  the executable is built and its launcher exits 2 before the
                            program runs, with TEXT on standard error and nothing printed
  PATH killed SIGNAL        the executable is built and the launcher's own limit ends the run
                            with SIGNAL (SIGALRM: its deadline) and nothing printed
  PATH differs TEXT         a known difference of the native target (TEXT says which): both
                            builds must exit 0 and print the same, which is not FIXTURE.expected
  PATH accepted TEXT        a .refused.wy the native target accepts (TEXT says why): both
                            builds must exit 0 and print the same
  PATH skipped TEXT         not built or run (TEXT says why: an outcome that varies from run
                            to run); counted as skipped

A listed fixture that does what an unlisted one must is a failure too (the line is stale). A
build or run that reaches its time limit is reported as a timeout and fails the run. A run
whose launcher refuses to start it (`native I/O requires ...`: a broker, or a listening port
not yet free) is started again after a wait, and the line says so; a program that ran and
failed is not run again.

Unless --no-control is given, the run ends with the false proof controls, one for each kind of
test the default build leaves out: a function whose proof Lean refuses reads an empty cell
with `memory.get_unchecked`, adds past the top of U64, or reads a slice through a length that
is wrong. The refused goal is then written into a cache of its own as accepted, as a wrong
proof would be. Checked mode must trap, and the default build must run on (or the control
shows nothing).
"""
import argparse, concurrent.futures, os, re, signal, subprocess, sys, tempfile, time
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--compiler', type=Path, required=True)
p.add_argument('--binding', type=Path, required=True)
cache_options = p.add_mutually_exclusive_group()
cache_options.add_argument('--proof-cache', type=Path)
cache_options.add_argument('--private-proof-cache', action='store_true',
                           help='share a fresh private proof cache across fixture builds')
p.add_argument('--measure', action='store_true', help='print build, run and retry timings to stderr')
p.add_argument('--jobs', type=int, default=4)
p.add_argument('--target', default='macos-arm64')
p.add_argument('--list', type=Path, default=Path(__file__).with_name('native-checked.list'))
p.add_argument('--no-control', action='store_true')
p.add_argument('--write-list', type=Path, help='write a list line for each unlisted fixture that fails in a way a line can describe')
p.add_argument('fixtures', nargs='*', type=Path)
a = p.parse_args()
repo = Path(__file__).resolve().parents[1]
compiler = a.compiler.resolve()
base = {k: v for k, v in os.environ.items() if not k.startswith(('WITCHY_', 'LEAN_', 'LAKE_'))}
base['WITCHY_WASM_READY_BINDING'] = str(a.binding.resolve())
cached = dict(base)
if a.proof_cache:
    a.proof_cache.mkdir(parents=True, exist_ok=True)
    cached['WITCHY_PROOF_CACHE'] = str(a.proof_cache.resolve())
WAYS = (('default', {}), ('checked', {'WITCHY_NATIVE_CHECKED': '1'}), ('observed', {'WITCHY_NATIVE_OBSERVED': '1'}))
TRAPS = (-signal.SIGILL, -signal.SIGTRAP)

listed = {}
for number, line in enumerate(a.list.read_text().splitlines() if a.list.exists() else [], 1):
    if not line.strip() or line.startswith('#'):
        continue
    words = line.split(None, 2)
    if len(words) != 3 or words[1] not in ('build-refused', 'launch-refused', 'killed', 'differs', 'accepted', 'skipped'):
        sys.exit(f'{a.list}:{number}: expected `PATH build-refused|launch-refused|killed|differs|accepted|skipped TEXT`')
    if words[1] == 'killed' and not isinstance(getattr(signal, words[2].split()[0], None), signal.Signals):
        sys.exit(f'{a.list}:{number}: `{words[2].split()[0]}` is not a signal')
    if not (repo / 'tests/fixtures' / words[0]).is_file():
        sys.exit(f'{a.list}:{number}: no fixture {words[0]}')
    listed[words[0]] = (words[1], words[2])

def name(fixture):
    return str(fixture.resolve().relative_to(repo / 'tests/fixtures'))

# The Wasm host's metering, which the native launcher has no counterpart for: a run is given
# the rest of its `run:` flags.
METERING = ('--unmetered', '--fuel=', '--memory-pages=', '--module-bytes=')

def run_flags(fixture, scratch):
    words = []
    flags = fixture.with_suffix('.flags')
    if flags.exists():
        for line in flags.read_text().splitlines():
            kind, _, rest = line.partition(':')
            if kind.strip() == 'run':
                scratch.mkdir(exist_ok=True)
                words += rest.replace('{dir}', str(fixture.parent.resolve())).replace('{scratch}', str(scratch)).split()
    return [word for word in words if not word.startswith(METERING)]

def messages(stderr):
    # As scripts/run-fixtures.py: only the messages count, not the path or the source line.
    return '\n'.join(re.findall(r'W\d{4}(?: \d+\.\.\d+| \([^)]*\))?: (.*)', stderr)
                     + [line for line in stderr.splitlines() if line.startswith(('proof failed: ', '  not proved: '))])

def build(fixture, exe, extra, env=None):
    """('built',), ('refused', stderr) or ('timeout',)."""
    try:
        started = time.monotonic()
        built = subprocess.run([compiler, 'build', fixture, '-o', exe, f'--target={a.target}',
                                *(['--measure'] if a.measure else [])],
                               env={**(env or cached), **extra}, capture_output=True, text=True, timeout=600)
    except subprocess.TimeoutExpired:
        if a.measure:
            print(f'build {fixture} {exe.name} timeout {time.monotonic() - started:.6f}s',
                  file=sys.stderr, flush=True)
        return ('timeout',)
    if a.measure:
        print(f'build {fixture} {exe.name} status={built.returncode} {time.monotonic() - started:.6f}s',
              file=sys.stderr, flush=True)
        if built.returncode == 0 and built.stdout:
            print(f'phases {fixture} {exe.name} {built.stdout.strip()}', file=sys.stderr, flush=True)
    if built.returncode == 0 and exe.is_file():
        return ('built',)
    if exe.exists():
        return ('refused', 'the build failed and left an executable\n' + built.stderr)
    return ('refused', built.stderr)

def execute(fixture, exe, scratch):
    """(status, stdout, stderr) or ('timeout',)."""
    asks = re.search(r'fn main\([^)]*Allocator', fixture.read_text())
    try:
        ran = subprocess.run([exe, '--output', *(['--allocator'] if asks else []), *run_flags(fixture, scratch)],
                             env={**base, 'WITCHY_HEAP_AUDIT': '1'}, capture_output=True, timeout=60)
    except subprocess.TimeoutExpired:
        return ('timeout',)
    return (ran.returncode, ran.stdout, ran.stderr.replace(str(exe).encode(), b'EXE'))

def show(result):
    return str(result)[:500]

def one(index, fixture, scratch):
    """(outcome, line, suggestion): outcome is pass, fail, timeout or the list's kind."""
    key = name(fixture)
    expected_file = fixture.with_suffix('.expected')
    expected = expected_file.read_bytes()
    kind = 'refused' if fixture.name.endswith('.refused.wy') else 'traps' if fixture.name.endswith('.traps.wy') else 'runs'
    rule = listed.get(key)
    if rule and rule[0] == 'skipped':
        return 'skipped', f'skipped        {key} ({rule[1]})', None
    results, retried = {}, []
    for way, extra in WAYS:
        exe = Path(scratch) / f'{index}.{way}'
        built = build(fixture, exe, extra)
        if built[0] == 'timeout':
            return 'timeout', f'TIMEOUT {key}: the {way} build', None
        if built[0] == 'refused':
            results[way] = built
        else:
            results[way] = None
            # The native launcher cannot listen again on a port a run has just closed until the
            # system frees it (about 30 s on macOS), and each build's run uses the fixture's port.
            waits = (5, 15, 30) if re.search(r'--listen=\S+:[1-9]', ' '.join(run_flags(fixture, Path(scratch) / f'{index}.flags'))) else (1, 2)
            for attempt in range(len(waits) + 1):
                started = time.monotonic()
                results[way] = execute(fixture, exe, Path(scratch) / f'{index}.{way}.{attempt}.files')
                if a.measure:
                    print(f'run {key} {way} attempt {attempt + 1} status={results[way][0]} '
                          f'{time.monotonic() - started:.6f}s',
                          file=sys.stderr, flush=True)
                if results[way][0] == 'timeout':
                    return 'timeout', f'TIMEOUT {key}: the {way} run', None
                # Only the launcher's own refusal to start is tried again: a program that ran
                # and failed has failed.
                busy = results[way][0] == 2 and results[way][1] == b'' and b'native I/O requires' in results[way][2]
                if not busy or attempt == len(waits):
                    break
                retried.append(way)
                started = time.monotonic()
                time.sleep(waits[attempt])
                if a.measure:
                    print(f'retry {key} {way} {time.monotonic() - started:.6f}s', file=sys.stderr, flush=True)
        if kind == 'refused' and results[way][0] == 'refused' and not rule:
            break  # a refusal comes before any code is emitted: one build shows it
    def all_ways(test):
        return len(results) == len(WAYS) and all(test(r) for r in results.values())
    refusal = lambda text: lambda r: r[0] == 'refused' and text in r[1]
    ran = lambda r: r[0] == 0 and r[2] == b''
    # What the fixture's name asks of it.
    if kind == 'refused':
        first = results['default']
        missing = [line for line in expected.decode().splitlines() if line not in messages(first[1])] if first[0] == 'refused' else None
        natural = first[0] == 'refused' and not missing
    elif kind == 'traps':
        natural = all_ways(lambda r: r[0] in TRAPS and r[1] == b'')
    else:
        natural = all_ways(lambda r: ran(r) and r[1] == expected)
    if rule:
        how, text = rule
        if natural:
            return 'fail', f'FAIL {key}: listed `{how}` but does what its name asks: remove the line', None
        if how == 'build-refused':
            ok = all_ways(refusal(text))
        elif how == 'launch-refused':
            ok = all_ways(lambda r: r[0] == 2 and r[1] == b'' and text.encode() in r[2])
        elif how == 'killed':
            ok = all_ways(lambda r: r[0] == -getattr(signal, text.split()[0], 0) and r[1] == b'')
        else:  # differs, accepted: both builds run, and print the same
            ok = all_ways(ran) and len({r[1] for r in results.values()}) == 1 and (how == 'accepted') == (kind == 'refused')
        if ok:
            return how, f'{how:14} {key} ({text})', None
        return 'fail', f'FAIL {key}: listed `{how} {text}`\n' + '\n'.join(f'  {w}: {show(r)}' for w, r in results.items()), None
    if natural:
        return 'pass', f'pass {kind:7} {key}' + (f' (launch tried again: {", ".join(retried)})' if retried else ''), None
    # A line the list could hold for this failure, for --write-list (to be read before it is kept).
    suggestion = None
    texts = {r[1] if r[0] == 'refused' else r[2].decode(errors='replace') if r[0] == 2 else None for r in results.values()}
    if kind != 'refused' and all_ways(lambda r: r[0] == 'refused'):
        found = re.search(r'W\d{4}[^:]*: ((?:macos-arm64 native target: )?[^`\n]{0,60})', results['default'][1])
        if found: suggestion = f'{key} build-refused {found.group(1).strip()}'
    elif kind != 'refused' and all_ways(lambda r: r[0] == 2 and r[1] == b'') and len(texts) == 1:
        suggestion = f'{key} launch-refused {next(iter(texts)).strip().splitlines()[0][:60].strip()}'
    elif all_ways(ran) and len({r[1] for r in results.values()}) == 1:
        suggestion = f"{key} {'accepted' if kind == 'refused' else 'differs'} REASON"
    detail = f'missing {missing}' if kind == 'refused' and missing else ''
    return 'fail', f'FAIL {kind} {key} {detail}\n' + '\n'.join(f'  {w}: {show(r)}' for w, r in results.items()), suggestion

# Programs whose proof Lean refuses, each relying on the refused fact for one kind of test the
# default build leaves out: (name, what the refusal must mention, source).
CONTROLS = (
    ('cell', 'memory.get_unchecked', '''pub sealed type Held<T>:
    data: Buffer<T>

fn peek<T>(held: &Held<T>, index: U64) -> T:
    ensures memory.unchanged(held.data, old(held.data))
    proof:
        auto
    return memory.get_unchecked(&(*held).data, index)

fn main(out: Console<Write>, allocator: Allocator):
    let allocation: Result<Buffer<U64>, U64> = memory.try_alloc(&allocator, 4)
    match move allocation:
        Ok(buffer):
            let held = Held(move buffer)
            out.print_int(peek(&held, 2))
            let Held(data) = move held
            match memory.try_release(move data):
                Ok:
                    return
                Err(back):
                    unreachable
        Err(code):
            return
'''),
    ('arithmetic', 'may overflow', '''fn next(value: U64) -> U64:
    ensures result > value
    proof:
        auto
    return value + 1

fn main(out: Console<Write>):
    out.print_int(next(18446744073709551615))
'''),
    ('slice read', 'result.len()', '''import console
import vec

sealed type Bag:
    items: Vec<U64>

impl Bag:
    fn len(bag: &Bag) -> U64:
        return (*bag).items.len()

    fn view(bag: &Bag) -> &[U64]:
        ensures result.len() == bag.items.len()
        proof:
            auto
        let all = (*bag).items.as_slice()
        return all.part(0, 1)

    fn index(bag: &Bag, at: U64) -> U64:
        return bag.view()[at]

fn total(bag: &Bag) -> U64:
    var sum: U64 = 0
    for i in 0..bag.len():
        sum += bag[i] * (i + 1)
    return sum

fn main(console: Console<Write>, allocator: Allocator):
    let Ok(made): Vec<U64> = Vec.new(&allocator, 8) else Err(code):
        return
    var items = move made
    for x in [3, 1, 4, 1, 5]:
        let _ = items.push_mut(&allocator, *x)
    let bag = Bag(move items)
    console.print_int(total(&bag))
    let Bag(owned) = move bag
    owned.close()
'''),
    # A fifth value stored in a prefix buffer of four cells. The write lands in the slack the
    # allocator leaves after a 72-byte block, so the default build runs on and prints the value;
    # it then empties and releases the buffer.
    ('prefix cell', 'memory.init_unchecked', '''import console
pub sealed type Stack<T>:
    data: PrefixBuffer<T>
    length: U64
    capacity: U64
    invariant memory.prefix(data, length, capacity)

impl<T> Stack<T>:
    pub fn new(allocator: &Allocator, capacity: U64) -> Result<Stack<T>, U64>:
        ensures match result:
            Ok(output):
                output.length == 0
                output.capacity == capacity
            Err(code):
                true
        proof:
            auto
        let first: Result<PrefixBuffer<T>, U64> = memory.try_alloc_prefix(allocator, capacity)
        match move first:
            Ok(data):
                return Ok(Stack(move data, 0, capacity))
            Err(code):
                return Err(code)

    pub fn push_mut(stack: &mut Stack<T>, item: T):
        ensures stack.capacity == old(stack.capacity)
        proof:
            auto
        let index = (*stack).length
        if index < 5:
            memory.init_unchecked(&mut (*stack).data, index, item)
            (*stack).length = index + 1

    pub fn get(stack: &Stack<T>, index: U64) -> Result<T, U64>:
        proof:
            auto
        if index < (*stack).length:
            return Ok(memory.get_unchecked(&(*stack).data, index))
        return Err(1)

    pub fn end(input: Stack<T>):
        ensures memory.released(old(input.data))
        proof:
            auto
        let length = input.length
        var buffer = move input.data
        take_all(&mut buffer, length)
        release_empty(move buffer)

fn take_all<T>(buffer: &mut PrefixBuffer<T>, count: U64):
    requires count <= memory.capacity(buffer)
    requires memory.initialized_range(buffer, 0, count)
    ensures memory.empty_range(buffer, 0, count)
    ensures memory.unchanged_from(buffer, old(buffer), count)
    proof:
        auto
    memory.take_prefix_unchecked(buffer, count)
    return

fn release_empty<T: Any>(input: PrefixBuffer<T>):
    requires memory.empty_all(input)
    ensures memory.released(old(input))
    proof:
        auto
    match memory.try_release(move input):
        Ok:
            return
        Err(remainder):
            release_empty(move remainder)

fn main(out: Console<Write>, allocator: Allocator):
    let Ok(made): Stack<U64> = Stack.new(&allocator, 4) else Err(e):
        return
    var stack = move made
    for i in 0..5:
        stack.push_mut(1000 + i)
    match stack.get(4):
        Ok(value):
            out.print_int(value)
        Err(e):
            out.print_int(99)
    stack.end()
'''),
)

def control(scratch, number, name, mention, text):
    '''Whether checked mode traps where a proof that was never checked said no test is needed,
    and the default build, trusting it, does not.'''
    source = Path(scratch) / f'false-proof-{number}.wy'
    source.write_text(text)
    cache = Path(scratch) / f'false-proof-cache-{number}'
    cache.mkdir(mode=0o700)
    env = {**base, 'WITCHY_PROOF_CACHE': str(cache.resolve())}
    honest = build(source, Path(scratch) / f'false-proof-{number}.honest', {}, env)
    if honest[0] != 'refused' or 'W0402' not in honest[1] or mention not in honest[1]:
        return False, f'the proof was not refused: {show(honest)}'
    refused = cache / 'refused.v1'
    # A refused goal's line gives Lean's message; a line of the explanation gives numbers.
    keys = [line.split()[0] for line in refused.read_text().splitlines() if not re.fullmatch(r'\S+ [\d,]*', line)]
    if not keys:
        return False, 'no refused goal in the cache'
    refused.unlink()
    accepted = cache / 'accepted.v1'
    with open(accepted, 'a') as file:
        file.write(''.join(f'{key} axioms=propext\n' for key in keys))
    accepted.chmod(0o600)
    ran = {}
    for way, extra in WAYS:
        exe = Path(scratch) / f'false-proof-{number}.{way}'
        built = build(source, exe, extra, env)
        if built[0] != 'built':
            return False, f'with the goal written in as accepted, the {way} build failed: {show(built)}'
        ran[way] = execute(source, exe, Path(scratch) / f'false-proof-{number}.files')
    report = '; '.join(f'{way}: ' + ('timeout' if r[0] == 'timeout' else f'status {r[0]}, printed {r[1]!r}') for way, r in ran.items())
    if not (ran['checked'][0] in TRAPS and ran['checked'][1] == b''):
        return False, 'checked mode did not trap; ' + report
    if name == 'prefix cell':
        # The one control whose access is to dense storage, which the observed build judges too.
        if not (ran['observed'][0] in TRAPS and ran['observed'][1] == b''):
            return False, 'the observed build did not trap; ' + report
        if ran['default'][:2] != (0, b'1004\n'):
            return False, 'the default build did not run on: the control no longer tells them apart; ' + report
        return True, report
    if ran['default'][0] != 0:
        return False, 'the default build did not run on: the control no longer tells the two apart; ' + report
    return True, report

counts, suggestions, failed = {}, [], False
missing = [str(f) for f in a.fixtures if not f.is_file()]
if missing or not a.fixtures and a.no_control:
    sys.exit('no such fixture: ' + ', '.join(missing) if missing else 'nothing to check')
fixtures = [f for f in a.fixtures if f.with_suffix('.expected').exists()]
with tempfile.TemporaryDirectory(prefix='witchy-checked-') as scratch:
    scratch = str(Path(scratch).resolve())  # a directory grant names its real path
    if a.private_proof_cache:
        proof_cache = Path(scratch) / 'proofs'
        proof_cache.mkdir(mode=0o700)
        cached['WITCHY_PROOF_CACHE'] = str(proof_cache)
    with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as pool:
        for outcome, line, suggestion in pool.map(lambda pair: one(pair[0], pair[1], scratch), enumerate(fixtures)):
            counts[outcome] = counts.get(outcome, 0) + 1
            if suggestion: suggestions.append(suggestion)
            print(line, flush=True)
    if not a.no_control:
        for number, (name, mention, text) in enumerate(CONTROLS):
            ok, detail = control(scratch, number, name, mention, text)
            print(f"{'pass' if ok else 'FAIL'} false proof control, {name}: {detail}", flush=True)
            failed |= not ok
if a.write_list and suggestions:
    a.write_list.write_text('\n'.join(sorted(suggestions)) + '\n')
get = lambda k: counts.get(k, 0)
print(f"{get('pass')} passed, {get('fail')} failed, {get('timeout')} timed out; not run as on Wasm and not counted as passed: "
      f"{get('build-refused')} build-refused, {get('launch-refused')} launch-refused, {get('killed')} killed, {get('differs')} differ, {get('accepted')} accepted, {get('skipped')} skipped; "
      f"{len(a.fixtures) - len(fixtures)} files without .expected not run")
sys.exit(1 if failed or get('fail') or get('timeout') else 0)
