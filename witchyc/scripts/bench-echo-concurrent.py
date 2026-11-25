#!/usr/bin/env python3
"""Concurrent TCP echo: two Witchy servers, examples/echo/server.wy (an event loop) and
examples/echo/tasks.wy (a task per connection, sequential code that pauses), against a
native Go server with a goroutine per connection, driven by one Go load client.

  scripts/bench-echo-concurrent.py --compiler BIN --runtime HOST [--binding FILE]
      [--proof-cache DIR] [--repeat N]

Each case is C connections, each doing R round trips of a P-byte payload, every echo
checked byte for byte by the client. Witchy runs one thread (one `poll` loop, unmetered;
the task server with a 4 MiB heap);
Go runs with its defaults and again with GOMAXPROCS=1. Reported: round trips per second
and p50/p99 latency in microseconds, the median of the repetitions.
"""
import argparse, os, subprocess, sys, tempfile, time
from pathlib import Path

CASES = [(1, 4000, 64), (32, 1000, 64), (128, 250, 64), (32, 250, 4096)]


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--compiler', type=Path, required=True)
    p.add_argument('--runtime', type=Path, required=True)
    p.add_argument('--binding', type=Path)
    p.add_argument('--proof-cache', type=Path)
    p.add_argument('--repeat', type=int, default=3)
    p.add_argument('--port', type=int, default=47950)
    a = p.parse_args()
    repo = Path(__file__).resolve().parents[1]
    env = dict(os.environ)
    if a.binding: env['WITCHY_WASM_READY_BINDING'] = str(a.binding)
    if a.proof_cache: env['WITCHY_PROOF_CACHE'] = str(a.proof_cache.resolve())
    with tempfile.TemporaryDirectory(prefix='witchy-echo-c-') as scratch:
        scratch = Path(scratch)
        module = scratch / 'server.wasm'
        tasks = scratch / 'tasks.wasm'
        for source, out, flags in [('server.wy', module, []), ('tasks.wy', tasks, ['--heap-pages=64', '--slots=1024'])]:
            built = subprocess.run([a.compiler, 'wasm-checked', repo / 'examples/echo' / source, '-o', out, *flags], env=env, capture_output=True, text=True)
            if built.returncode != 0: sys.exit(f'{source} did not compile:\n{built.stderr}')
        goenv = dict(env, GOCACHE=str(scratch / 'gocache'), GOPATH=str(scratch / 'gopath'), GOFLAGS='-mod=mod')
        for name in ['go-client', 'go-server']:
            source = scratch / name
            source.mkdir()
            (source / 'main.go').write_text((repo / 'examples/echo' / name / 'main.go').read_text())
            (source / 'go.mod').write_text(f'module {name}\n\ngo 1.21\n')
            done = subprocess.run(['go', 'build', '-o', scratch / name / 'bin', '.'], cwd=source, env=goenv, capture_output=True, text=True)
            if done.returncode != 0: sys.exit(f'go build {name} failed:\n{done.stderr}')
        client = scratch / 'go-client' / 'bin'
        servers = {
            'witchy (1 thread)': lambda port, c: [a.runtime, module, '--output', '--allocator', '--unmetered', '--deadline-ms=600000',
                                                  '--net-bytes=268435456', f'--listen=127.0.0.1:{port}', f'--grant=Load.connections={c}'],
            'witchy tasks': lambda port, c: [a.runtime, tasks, '--output', '--allocator', '--unmetered', '--deadline-ms=600000', '--memory-pages=128',
                                             '--net-bytes=268435456', f'--listen=127.0.0.1:{port}', f'--grant=Load.connections={c}'],
            'go': lambda port, c: [scratch / 'go-server' / 'bin', f'127.0.0.1:{port}', str(c)],
            'go GOMAXPROCS=1': lambda port, c: [scratch / 'go-server' / 'bin', f'127.0.0.1:{port}', str(c)],
        }
        results = {}
        port = a.port
        for _ in range(a.repeat):
            for case in CASES:
                c, r, size = case
                for name, argv in servers.items():
                    port += 1
                    server_env = dict(env, GOMAXPROCS='1') if 'GOMAXPROCS' in name else env
                    server = subprocess.Popen(argv(port, c), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=server_env)
                    time.sleep(0.2)
                    ran = subprocess.run([client, f'127.0.0.1:{port}', str(c), str(r), str(size)], capture_output=True, text=True, timeout=300)
                    out, err = server.communicate(timeout=60)
                    if ran.returncode != 0 or server.returncode != 0 or out.strip() != str(c * r * size):
                        sys.exit(f'{name} {case}: client {ran.returncode} {ran.stderr.strip()}; server {server.returncode} {out.strip()!r} {err[-400:]}')
                    _, rate, p50, p99 = map(float, ran.stdout.split())
                    results.setdefault((case, name), []).append((rate, p50, p99))
    print(f"{'case':28} {'server':18} {'round trips/s':>14} {'p50 us':>8} {'p99 us':>8}")
    for (case, name), runs in results.items():
        median = lambda k: sorted(x[k] for x in runs)[len(runs) // 2]
        c, r, size = case
        print(f'{f"{c} conns x {r} x {size} B":28} {name:18} {median(0):14.0f} {median(1):8.0f} {median(2):8.0f}')


if __name__ == '__main__':
    main()
