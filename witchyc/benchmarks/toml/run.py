#!/usr/bin/env python3
"""Time one TOML parse: Witchy against toml-spanner, toml and toml_edit.

    run.py --witchy EXE [--rust rust/target/release/toml-compare] [--go go/toml-go] [--rounds 9] [--seconds 0.3]

EXE is bench.wy built for the native target (`witchyc build bench.wy -o EXE --target=...`);
the Rust program is `cargo build --release` in rust/, the Go one `go build -o toml-go .` in go/
(pelletier/go-toml v2 and BurntSushi/toml, each into a `map[string]any`). Either may be left out. Each input is parsed `count` times in one
process, with `count` chosen so a run takes about `--seconds`. Every round runs each parser
once and its baseline once, one after the other (Witchy's baseline copies the text and does
not parse it; the Rust one reads the file and does not parse it), so drift in the machine's
load falls on all of them alike. The time of a parse is the median over rounds of
(run - baseline) / count. Prints a Markdown table.
"""
import argparse, os, shutil, statistics, subprocess, tempfile, time

p = argparse.ArgumentParser()
p.add_argument('--witchy', required=True); p.add_argument('--rust'); p.add_argument('--go')
p.add_argument('--rounds', type=int, default=9); p.add_argument('--seconds', type=float, default=0.3)
p.add_argument('--inputs', nargs='*', default=['zed', 'zed-lock', 'extask', 'devsm', 'big'])
a = p.parse_args()
here = os.path.dirname(os.path.abspath(__file__))
PARSERS = ['witchy'] + (['spanner', 'toml', 'toml_edit'] if a.rust else []) + (['go_pelletier', 'go_ast', 'go_burntsushi'] if a.go else [])
BASIS = 'go_pelletier' if a.go else 'spanner'

def timed(command):
    start = time.perf_counter()
    done = subprocess.run(command, capture_output=True)
    if done.returncode != 0: raise SystemExit(f'failed: {command}: {done.stderr.decode()[:200]}')
    return time.perf_counter() - start

def commands(parser, source, count, work):
    """The run that parses and the run that does everything but parse."""
    if parser == 'witchy':
        both = []
        for kind in ('parse', 'copy'):
            folder = os.path.join(work, kind); os.makedirs(folder, exist_ok=True)
            shutil.copy(source, os.path.join(folder, 'input.toml'))
            open(os.path.join(folder, 'count'), 'w').write(str(count))
            if kind == 'copy': open(os.path.join(folder, 'copy-only'), 'w').write('')
            both.append([a.witchy, '--output', '--allocator', f'--dir={folder}'])
        return both
    if parser.startswith('go_'):
        return [[a.go, parser[3:], source, str(count)], [a.go, 'none', source, str(count)]]
    return [[a.rust, parser, source, str(count)], [a.rust, 'none', source, str(count)]]

print('| input | bytes | ' + ' | '.join(PARSERS) + f' | witchy / {BASIS} |')
print('|---|---|' + '---|' * (len(PARSERS) + 1))
for name in a.inputs:
    source = os.path.join(here, 'data', name + '.toml')
    work = os.path.realpath(tempfile.mkdtemp(prefix='toml-bench-'))
    plan = {}
    for parser in PARSERS:
        count = 4
        while True:  # enough parses for the run to take about `seconds`
            run, base = commands(parser, source, count, os.path.join(work, parser))
            took = timed(run) - timed(base)
            if took >= a.seconds / 2 or count >= 1 << 24: break
            count = min(1 << 24, max(count * 2, int(count * a.seconds / max(took, 1e-4))))
        plan[parser] = (count, run, base)
    samples = {parser: [] for parser in PARSERS}
    for _ in range(a.rounds):
        for parser in PARSERS:
            count, run, base = plan[parser]
            samples[parser].append((timed(run) - timed(base)) / count * 1e6)
    shutil.rmtree(work)
    median = {parser: statistics.median(samples[parser]) for parser in PARSERS}
    cells = ' | '.join(f'{median[parser]:.1f} µs' for parser in PARSERS)
    print(f'| {name} | {os.path.getsize(source)} | {cells} | {median["witchy"] / median[BASIS]:.2f} |', flush=True)
