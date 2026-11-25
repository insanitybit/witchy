#!/usr/bin/env python3
"""Synchronous TCP echo: Witchy (bulk and byte-at-a-time host I/O) against native Go.

  scripts/bench-echo.py --compiler BIN --runtime HOST [--binding FILE] [--proof-cache DIR]
      [--rounds N] [--repeat N]

Each server takes one connection on 127.0.0.1 and echoes until the client closes. One
client (this script) sends a payload, reads it all back, checks every byte, and repeats:
round trips of 64 bytes (latency) and 4 KiB, then one 4 MiB stream (throughput). The same
client drives every server, so its own cost is in every number alike. Witchy runs
unmetered (no instruction count), with a raised network budget; Go is `go build` defaults.
This is a baseline for later concurrent servers, not a claim about either language.
"""
import argparse, os, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path

BULK = '''import console
import net
import vec

fn echo(socket: &Socket, buffer: &mut [U8]) -> Result<U64, NetError>:
    var total: U64 = 0
    var more = true
    while more:
        let n = socket.read_some(buffer)?
        if n == 0:
            more = false
        else:
            match socket.write_all(buffer.part(0, n)):
                Ok:
                    total += n
                Err(failure):
                    return Err(failure.error)
    return Ok(total)

fn main(console: Console<Write>, listener: Listener, allocator: Allocator):
    let zero: U8 = 0
    let Ok(made): Vec<U8> = Vec.filled(&allocator, 16384, zero) else Err(code):
        return
    var buffer = move made
    match listener.accept():
        Ok(socket):
            match echo(&socket, buffer.as_mut_slice()):
                Ok(total):
                    console.print_int(total)
                Err(e):
                    console.print_int(0)
            socket.close()
        Err(e):
            console.print_int(1)
    buffer.close()
'''

BYTES = '''import console
import net

fn main(console: Console<Write>, listener: Listener):
    match listener.accept():
        Ok(socket):
            var total: U64 = 0
            var more = true
            while more:
                match socket.next_byte():
                    Ok(byte):
                        let code = socket.raw_send_byte(byte)
                        total += 1
                    Err:
                        more = false
            console.print_int(total)
            socket.close()
        Err(e):
            console.print_int(1)
'''

GO = '''package main

import (
	"fmt"
	"net"
	"os"
)

func main() {
	listener, err := net.Listen("tcp", os.Args[1])
	if err != nil { panic(err) }
	connection, err := listener.Accept()
	if err != nil { panic(err) }
	buffer := make([]byte, 16384)
	total := 0
	for {
		n, err := connection.Read(buffer)
		if n > 0 {
			if _, werr := connection.Write(buffer[:n]); werr != nil { panic(werr) }
			total += n
		}
		if err != nil { break }
	}
	connection.Close()
	fmt.Println(total)
}
'''


def connect(address, deadline):
    host, port = address.rsplit(':', 1)
    while True:
        try:
            s = socket.create_connection((host, int(port)), timeout=5)
            s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            return s
        except OSError:
            if time.monotonic() > deadline: raise
            time.sleep(0.01)


def exchange(s, payload, rounds):
    """`rounds` round trips of `payload`, each echo checked byte for byte."""
    started = time.perf_counter()
    for _ in range(rounds):
        # Send from a second thread: a payload larger than the socket buffers would otherwise
        # block both sides, each waiting for the other to read.
        sender = threading.Thread(target=s.sendall, args=(payload,))
        sender.start()
        got = bytearray()
        while len(got) < len(payload):
            chunk = s.recv(len(payload) - len(got))
            if not chunk: raise RuntimeError('server closed early')
            got += chunk
        sender.join()
        if got != payload: raise RuntimeError('echo differs')
    return time.perf_counter() - started


def run(argv, address, cases):
    server = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        s = connect(address, time.monotonic() + 1.5)
        try:
            timings = {label: exchange(s, payload, rounds) for label, payload, rounds in cases}
        except (OSError, RuntimeError) as e:
            server.kill()
            raise RuntimeError(f'{argv[1]}: {e}; server said: {server.communicate()[1][-800:]}')
        s.close()
        out, err = server.communicate(timeout=30)
        expected = sum(len(payload) * rounds for _, payload, rounds in cases)
        if server.returncode != 0 or out.strip() != str(expected):
            raise RuntimeError(f'server: exit {server.returncode}, printed {out.strip()!r}, expected {expected}\n{err}')
        return timings
    finally:
        if server.poll() is None: server.kill()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--compiler', type=Path, required=True)
    p.add_argument('--runtime', type=Path, required=True)
    p.add_argument('--binding', type=Path)
    p.add_argument('--proof-cache', type=Path)
    p.add_argument('--rounds', type=int, default=4000)
    p.add_argument('--repeat', type=int, default=3)
    p.add_argument('--port', type=int, default=47801)
    a = p.parse_args()
    env = dict(os.environ)
    if a.binding: env['WITCHY_WASM_READY_BINDING'] = str(a.binding)
    if a.proof_cache: env['WITCHY_PROOF_CACHE'] = str(a.proof_cache.resolve())
    address = f'127.0.0.1:{a.port}'
    pattern = bytes(i % 251 for i in range(4 << 20))
    cases = [('64 B round trips', pattern[:64], a.rounds), ('4 KiB round trips', pattern[:4096], a.rounds // 4),
             ('4 MiB stream', pattern, 1)]
    with tempfile.TemporaryDirectory(prefix='witchy-echo-') as scratch:
        scratch = Path(scratch)
        servers = {}
        for name, source in [('witchy bulk', BULK), ('witchy byte-at-a-time', BYTES)]:
            path = scratch / f"{name.split()[-1]}.wy"
            path.write_text(source)
            module = path.with_suffix('.wasm')
            built = subprocess.run([a.compiler, 'wasm-checked', path, '-o', module], env=env, capture_output=True, text=True)
            if built.returncode != 0: sys.exit(f'{name} did not compile:\n{built.stderr}')
            servers[name] = [a.runtime, module, '--output', '--unmetered', '--deadline-ms=600000', '--net-bytes=268435456',
                             f'--listen={address}', *(['--allocator'] if 'Allocator' in source else [])]
        (scratch / 'main.go').write_text(GO)
        built = subprocess.run(['go', 'build', '-o', scratch / 'echo-go', scratch / 'main.go'], capture_output=True, text=True,
                               env=dict(env, GOCACHE=str(scratch / 'gocache'), GOPATH=str(scratch / 'gopath')))
        if built.returncode != 0: sys.exit(f'go build failed:\n{built.stderr}')
        servers['go'] = [scratch / 'echo-go', address]
        results = {name: [] for name in servers}
        for _ in range(a.repeat):
            for name, argv in servers.items():  # alternating order, every repetition
                results[name].append(run(argv, address, cases))
    print(f"{'server':24} " + ' '.join(f'{label:>20}' for label, _, _ in cases))
    for name, runs in results.items():
        cells = []
        for label, payload, rounds in cases:
            best = sorted(r[label] for r in runs)[len(runs) // 2]  # median
            per = best / rounds * 1e6
            rate = len(payload) * rounds / best / 1e6
            cells.append(f'{per:9.1f} us {rate:6.1f} MB/s')
        print(f'{name:24} ' + ' '.join(f'{c:>20}' for c in cells))


if __name__ == '__main__':
    main()
