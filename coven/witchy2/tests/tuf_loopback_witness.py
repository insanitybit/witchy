"""Exercise the bounded TUF loopback worker with an independent HTTP peer."""
from concurrent.futures import ThreadPoolExecutor
import json
import os
import signal
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
from unittest.mock import patch
from urllib.parse import urlsplit

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
from coven_worker import STAGED_FILE_ENTRY_BYTES
from coven_https_client import DOWNLOAD_BYTES, LOOPBACK_BUILD, Download, DownloadRefused


def read_request(stream, address):
    assert stream.readline(1024) == b"GET /metadata/timestamp.json HTTP/1.1\r\n"
    assert stream.readline(1024) == f"Host: {address}\r\n".encode()
    assert stream.readline(1024) == b"Connection: close\r\n"
    assert stream.readline(1024) == b"Accept-Encoding: identity\r\n"
    assert stream.readline(1024) == b"\r\n"


def exchange(work, module, reply, expected=None, *, max_length=None, stalled=False):
    with socket.create_server(("127.0.0.1", 0)) as listener, ThreadPoolExecutor(max_workers=1) as pool:
        listener.settimeout(3)
        address = f"127.0.0.1:{listener.getsockname()[1]}"

        def respond():
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(3)
                with connection.makefile("rb") as stream:
                    read_request(stream, address)
                    connection.sendall(reply)
                    if stalled:
                        assert stream.read(1) == b""

        peer = pool.submit(respond)
        url = urlsplit(f"http://{address}")
        client = Download(work, (str(module),), None, os.environ["WITCHY_WASMTIME_HOST"], url)
        if stalled:
            client.command = (*client.command[:-1], "--deadline-ms=500")
        output = work / "transfer"
        try:
            client.fetch("/metadata/timestamp.json", output, length=expected, max_length=max_length)
        except DownloadRefused as error:
            assert not output.exists()
            return error.status
        except ValueError as error:
            assert str(error) == "Witchy TUF download refused", error
            assert not output.exists()
            return None
        finally:
            peer.result(timeout=4)
        payload = output.read_bytes()
        output.unlink()
        return payload


def cancel_download(work, module):
    output = work / "cancelled"
    with socket.create_server(("127.0.0.1", 0)) as listener:
        listener.settimeout(4)
        url = f"http://127.0.0.1:{listener.getsockname()[1]}"
        with subprocess.Popen([sys.executable, __file__, "--download", module, url, output],
                              start_new_session=True) as launcher:
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(2)
                    with connection.makefile("rb") as stream:
                        read_request(stream, urlsplit(url).netloc)
                        connection.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                        launcher.kill()
                        assert launcher.wait(timeout=2) == -signal.SIGKILL
                        # The socket's own read timeout is two seconds.
                        connection.settimeout(1)
                        assert stream.read(1) == b"", "download socket outlived the launcher"
                assert not output.exists(), "cancelled download published partial data"
            except BaseException:
                try:
                    os.killpg(launcher.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
                raise


def check_file_grants(work):
    anchors = work / "fixture-ca.pem"
    anchors.write_bytes(b"fixture CA")
    file_bytes = anchors.stat().st_size
    ok = subprocess.CompletedProcess([], 0, b"ok\n", b"")
    for ca, url, trust_bytes in ((None, "http://127.0.0.1:1", 0), (anchors, "https://localhost", file_bytes)):
        client = Download(work, ("test-worker.wasm",), ca, "test-host", urlsplit(url))
        with patch("coven_https_client.run_with_lifeline", return_value=ok) as run:
            for limits, upper_bound in (({}, DOWNLOAD_BYTES), ({"length": 42}, 42), ({"max_length": 23}, 23)):
                client.fetch("/metadata/timestamp.json", work / "target", **limits)
                command = run.call_args.args[1]
                fs_flags = [arg for arg in command if arg.startswith("--fs-")]
                assert fs_flags == ["--fs-entries=1", f"--fs-bytes={upper_bound + trust_bytes + STAGED_FILE_ENTRY_BYTES}"]
                request = json.loads(run.call_args.kwargs["input"])
                assert request == {"origin": url, "path": "/metadata/timestamp.json", **limits}
            run.reset_mock()
            for limits in ({"length": True}, {"length": 0}, {"max_length": DOWNLOAD_BYTES + 1},
                           {"length": 1, "max_length": 2}):
                try:
                    client.fetch("/metadata/timestamp.json", work / "target", **limits)
                except ValueError:
                    pass
                else:
                    raise AssertionError(f"invalid transfer limits accepted: {limits}")
            run.assert_not_called()


def check(work):
    check_file_grants(work)
    module = work / "download.wasm"
    subprocess.run([os.environ["WITCHYC"], "build", HERE / "tuf_get_loopback.wy", "-o", module, *LOOPBACK_BUILD],
                   check=True, timeout=30, stdout=subprocess.DEVNULL)
    assert exchange(work, module, b"HTTP/1.0 200 OK\r\n\r\n\x00\xff", 2) == b"\x00\xff"
    assert exchange(work, module, b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n", 2) == b"ok"
    assert exchange(work, module, b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n") == 404
    assert exchange(work, module, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx", 2) is None
    assert exchange(work, module, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n", 1) is None
    assert exchange(work, module, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", max_length=2) == b"ok"
    assert exchange(work, module, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", max_length=1) is None
    assert exchange(work, module, b"HTTP/1.0 200 OK\r\n\r\nok", max_length=3) == b"ok"
    assert exchange(work, module, b"HTTP/1.0 200 OK\r\n\r\nok", max_length=1) is None
    assert exchange(work, module, b"HTTP/1.1 200 OK\r\n", stalled=True) is None
    body = b"x" * DOWNLOAD_BYTES
    assert exchange(work, module, f"HTTP/1.1 200 OK\r\nContent-Length: {len(body)}\r\n\r\n".encode() + body) == body
    cancel_download(work, module)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--download"]:
        _, _, module, url, output = sys.argv
        destination = Path(output)
        client = Download(destination.parent, (module,), None, os.environ["WITCHY_WASMTIME_HOST"], urlsplit(url))
        client.fetch("/metadata/timestamp.json", destination, length=2)
        raise SystemExit(0)
    with tempfile.TemporaryDirectory(prefix="coven-loopback-") as directory:
        check(Path(directory).resolve())
    print("TUF loopback transport passed length, framing, deadline and launcher cancellation checks")
