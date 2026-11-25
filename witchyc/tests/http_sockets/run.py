"""Exercise the bounded Witchy HTTP socket APIs over loopback."""
import argparse
import os
from pathlib import Path
import select
import socket
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent


def build(source, output, native):
    target = ["--target=macos-arm64"] if native else []
    subprocess.run([os.environ["WITCHYC"], "build", source, "-o", output, *target],
                   check=True, stdout=subprocess.DEVNULL, timeout=30)


def host(program, native, *flags):
    command = [program] if native else [os.environ["WITCHY_WASMTIME_HOST"], program, "--fuel=8000000"]
    return subprocess.Popen([*command, "--deadline-ms=3000", *flags],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def check_client(work, native, name, cases, *grants, method="GET", prefix=b"HTTP/1.1 200 OK\r\n"):
    with socket.create_server(("127.0.0.1", 0)) as listener:
        listener.settimeout(3)
        address = f"127.0.0.1:{listener.getsockname()[1]}"
        source = work / f"{name}.wy"
        source.write_text((HERE / source.name).read_text().replace("TEST_ADDRESS", address))
        program = work / (name if native else f"{name}.wasm")
        build(source, program, native)
        for index, (wire, expected, eof) in enumerate(cases, start=1):
            with host(program, native, "--output", f"--net={address}", *grants) as child:
                conn, _ = listener.accept()
                with conn:
                    conn.settimeout(3)
                    request = bytearray()
                    while b"\r\n\r\n" not in request:
                        chunk = conn.recv(256)
                        assert chunk, request
                        request.extend(chunk)
                        assert len(request) <= 256, request
                    assert request == f"{method} /sample HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n".encode()
                    conn.sendall(prefix + wire)
                    if eof:
                        conn.shutdown(socket.SHUT_WR)
                    output, errors = child.communicate(timeout=3)
                    assert (child.returncode, output) == (0, expected), (name, index, output, errors)


SOCKET_CASES = (
    (b"Content-Length: 3\r\n\r\nabc", b"ok\n", False),
    (b"Content-Length: 3\r\n\r\n\0\xff\x02", b"binary\n", False),
    (b"Transfer-Encoding: chunked\r\n\r\n1\r\na\r\n2\r\nbc\r\n0\r\n\r\n", b"ok\n", False),
    (b"Content-Length: 4\r\n\r\n", b"body limit\n", False),
    (b"Content-Length: 3\r\n\r\nab", b"incomplete response\n", True),
)

GET_CASES = (
    (b"Content-Length: 3\r\n\r\nabc", b"200\nabc", False),
    (b"Transfer-Encoding: chunked\r\n\r\n1\r\na\r\n2\r\nbc\r\n0\r\n\r\n", b"200\nabc", False),
    (b"Connection: close\r\n\r\nabc", b"200\nabc", True),
    (b"Content-Length: 3\r\n\r\n\0\xff\x02", b"non-text response\n", False),
    (b"Content-Length: 3\r\n\r\nab", b"malformed response\n", True),
    (b"Content-Length: 3\r\nContent-Length: 4\r\n\r\nabc", b"malformed response\n", False),
    (b"Content-Length: 16777217\r\n\r\n", b"malformed response\n", False),
    (b"Content-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n", b"malformed response\n", False),
    (b"X-Large: " + b"a" * 16384 + b"\r\n\r\n", b"malformed response\n", False),
)


HEAD_CASES = (
    (b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n", b"ok\n", False),
    (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n", b"ok\n", False),
    (b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200 OK\r\n\r\n", b"ok\n", False),
    (b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n", b"invalid response\n", False),
    (b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n", b"invalid response\n", False),
    (b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n", b"incomplete response\n", True),
)


def check_server(work, native):
    program = work / ("server" if native else "server.wasm")
    build(HERE / "server.wy", program, native)
    with host(program, native, "--instance-per-connection", "--listen=127.0.0.1:0", "--connections=3") as child:
        ready, _, _ = select.select([child.stderr], [], [], 3)
        assert ready, "listener did not start"
        address = child.stderr.readline().decode().strip()
        prefix = "witchy: listening on 127.0.0.1:"
        assert address.startswith(prefix), address
        port = int(address.removeprefix(prefix))
        for authority, length, body, reply in (
            (f"127.0.0.1:{port}", 3, "abc", b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
            ("other.test", 0, "", b"HTTP/1.1 421 Misdirected Request\r\nContent-Length: 0\r\n\r\n"),
            (f"127.0.0.1:{port}", 4, "", b"HTTP/1.1 413 Content Too Large\r\nContent-Length: 0\r\n\r\n"),
        ):
            with socket.create_connection(("127.0.0.1", port), timeout=3) as conn:
                conn.sendall(f"POST / HTTP/1.1\r\nHost: {authority}\r\nContent-Length: {length}\r\n\r\n{body}".encode())
                conn.shutdown(socket.SHUT_WR)
                with conn.makefile("rb") as stream:
                    assert stream.read(len(reply) + 1) == reply
        _, errors = child.communicate(timeout=3)
        assert child.returncode == 0, errors


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", action="store_true", help="use the macOS ARM64 native target")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as temporary:
        check_client(Path(temporary), args.native, "client", SOCKET_CASES)
        check_client(Path(temporary), args.native, "get_client", GET_CASES, "--allocator")
        check_client(Path(temporary), args.native, "head_client", HEAD_CASES, "--allocator", method="HEAD", prefix=b"")
        check_server(Path(temporary), args.native)
    print("bounded HTTP client and server loopback checks passed")
