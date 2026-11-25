"""Coven HTTP transport and whole-publication commit boundaries."""
import base64
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import select
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from cli_test_support import fresh_coven, invoke_coven, policy_signatures, protocol_http_server
import coven_http

HERE = Path(__file__).resolve().parent
COVEN = HERE / "coven"

# A loopback server with fixed routes, a test-supplied deadline, and refusals that name paths of
# the server's own.
FAKE_SERVER = """
import sys
sys.path.insert(0, sys.argv[1])
import coven_http
coven_http.REQUEST_SECONDS = float(sys.argv[3])
def routes(route):
    return {"bytes": "fixture-reply"} if route == ["packages"] else {}
def accept(store, route, body):
    coven_http.decode(body, ["field"])
    def refusal(tree):
        raise ValueError(f"refused {tree}/lib/index under " + store)
    return coven_http.transaction(store, refusal)
from cli_test_support import fresh_coven
from pathlib import Path
cli = fresh_coven(Path(sys.argv[1]) / "coven")
coven_http.serve(sys.argv[2], 0, routes, accept, (ValueError,), build=cli.build_module,
                 host=cli.tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))
"""


def start_server(case, command, log):
    """The server's URL; its log (refusals in full) is kept in `log`."""
    with open(log, "wb") as stream:
        server = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=stream, text=True)

    def stop():
        server.terminate()
        server.communicate(timeout=30)
    case.addCleanup(stop)
    line = server.stdout.readline()
    case.assertTrue(line.startswith("Coven listening at "), line or Path(log).read_text())
    return line.split()[-1]


def client_request(url, *arguments):
    cli = fresh_coven(COVEN)
    with coven_http.request_worker(url, cli.build_module, cli.tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")) as request:
        return request(*arguments)


def raw_exchange(port, data, timeout=10):
    with socket.create_connection(("127.0.0.1", port), timeout=timeout) as connection:
        connection.sendall(data)
        reply = b""
        while chunk := connection.recv(65536):
            reply += chunk
        return reply


class HttpStore(unittest.TestCase):
    def test_public_objects_are_bounded_before_materializing_the_response(self):
        with tempfile.TemporaryDirectory() as directory, closing(coven_http.database(directory, create=True)) as db:
            db.executemany("INSERT INTO objects VALUES(?,?)", [
                ("one", b"a" * 160), ("two", b"b" * 160), ("empty", b""),
                ("oversized", bytes(4096)), ("invalid", "text"),
            ])
            db.commit()
            db.execute("BEGIN")
            # A value above the SQLite limit cannot be returned by a direct SELECT.
            # It must still get the HTTP transport's expected refusal.
            db.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, 2048)
            with patch.object(coven_http, "LIMIT", 256):
                self.assertEqual(coven_http.read_public_objects(db, {"data": "one", "missing": "absent", "empty": "empty"}),
                                 {"data": b"a" * 160, "missing": None, "empty": b""})
                for names in ({"data": "oversized"}, {"first": "one", "second": "two"}):
                    with self.subTest(names=names), self.assertRaisesRegex(ValueError, "response exceeds the transport limit"):
                        coven_http.read_public_objects(db, names)
                with self.assertRaisesRegex(TypeError, "object is not binary"):
                    coven_http.read_public_objects(db, {"data": "invalid"})

    def test_refusal_cannot_partially_replace_a_committed_registry(self):
        with tempfile.TemporaryDirectory() as directory:
            coven_http.database(directory, create=True).close()
            coven_http.transaction(directory, lambda tree: (tree / "snapshot").write_bytes(b"first"))
            def refused(tree):
                (tree / "snapshot").write_bytes(b"second")
                (tree / "another").write_bytes(b"incomplete")
                raise ValueError("signature refused")
            with self.assertRaisesRegex(ValueError, "signature refused"):
                coven_http.transaction(directory, refused)
            self.assertEqual(coven_http.transaction(directory, lambda tree: {p.name: p.read_bytes() for p in tree.iterdir()}),
                             {"snapshot": b"first"})

    def test_streams_large_objects_under_the_sql_result_limit(self):
        with tempfile.TemporaryDirectory() as directory, closing(coven_http.database(directory, create=True)) as db:
            old = bytes(range(256)) * 600
            db.execute("INSERT INTO objects VALUES (?, ?)", ("large", old))
            db.commit()
            db.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, 2048)
            with self.assertRaises(sqlite3.DataError):
                db.execute("SELECT data FROM objects").fetchone()

            def bounded_connection(_store):
                connection = sqlite3.connect(Path(directory) / coven_http.DATABASE)
                connection.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, 2048)
                return connection

            def check_staged(tree):
                self.assertEqual((tree / "large").read_bytes(), old)
                (tree / "empty").touch()

            with patch.object(coven_http, "database", bounded_connection):
                coven_http.transaction(directory, check_staged)
            db.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, 1048576)
            self.assertEqual(dict(db.execute("SELECT name, data FROM objects")), {"large": old, "empty": b""})

    def test_replaces_and_deletes_objects_and_rolls_back_a_bad_export(self):
        with tempfile.TemporaryDirectory() as directory, closing(coven_http.database(directory, create=True)) as db:
            initial = {"keep": b"k", "change": b"before", "delete": b"x"}
            db.executemany("INSERT INTO objects VALUES (?, ?)", initial.items())
            db.commit()
            replacement = bytes(range(256)) * 600

            def edit(tree):
                (tree / "change").write_bytes(replacement)
                (tree / "delete").unlink()
                (tree / "add").write_bytes(b"new")

            def bad_export(tree):
                edit(tree)
                (tree / "late").mkdir()
                os.mkfifo(tree / "late" / "invalid")

            with self.assertRaisesRegex(ValueError, "non-private file"):
                coven_http.transaction(directory, bad_export)
            self.assertEqual(dict(db.execute("SELECT name, data FROM objects")), initial)
            coven_http.transaction(directory, edit)
            after = {"keep": b"k", "change": replacement, "add": b"new"}
            self.assertEqual(dict(db.execute("SELECT name, data FROM objects")), after)

    def test_package_scope_keeps_read_objects_and_unrelated_rows_out_of_writes(self):
        initial = {
            "snapshot": b"operator",
            "lib/index": b"current",
            "lib/1.0.0/digest": b"prior release",
            "leaf/1.0.0/digest": b"dependency",
            "leaf/1.0.0/source.wy": b"unneeded dependency contents",
            "lib-extra/1.0.0/source.wy": b"another package with a similar prefix",
        }
        scope = coven_http.PublicationScope("lib", "1.0.1", {"leaf": "1.0.0"})
        with tempfile.TemporaryDirectory() as directory, closing(coven_http.database(directory, create=True)) as db:
            db.executemany("INSERT INTO objects VALUES (?, ?)", initial.items())
            db.commit()

            def publish(tree):
                files = {path.relative_to(tree).as_posix() for path in tree.rglob("*") if path.is_file()}
                self.assertEqual(files, {"snapshot", "lib/index", "lib/1.0.0/digest", "leaf/1.0.0/digest"})
                (tree / "lib/index").write_bytes(b"next")
                target = tree / "lib/1.0.1"
                target.mkdir()
                (target / "digest").write_bytes(b"new release")

            def change_dependency(tree):
                publish(tree)
                (tree / "leaf/1.0.0/digest").write_bytes(b"changed")

            def remove_snapshot(tree):
                publish(tree)
                (tree / "snapshot").unlink()

            def change_older_slot(tree):
                publish(tree)
                (tree / "lib/1.0.0/digest").write_bytes(b"changed")

            def create_unexpected_slot_file(tree):
                publish(tree)
                (tree / "lib/1.0.1/unexpected").touch()

            for operation, message in (
                (change_dependency, "outside its slot and index"),
                (remove_snapshot, "removed a registry object"),
                (change_older_slot, "outside its slot and index"),
                (create_unexpected_slot_file, "outside its slot and index"),
            ):
                with self.subTest(operation=operation.__name__):
                    with self.assertRaisesRegex(ValueError, message):
                        coven_http.transaction(directory, operation, scope=scope)
                    self.assertEqual(dict(db.execute("SELECT name, data FROM objects")), initial)
            coven_http.transaction(directory, publish, scope=scope)
            expected = initial | {"lib/index": b"next", "lib/1.0.1/digest": b"new release"}
            self.assertEqual(dict(db.execute("SELECT name, data FROM objects")), expected)

    def test_damaged_catalog_refuses_invalid_rows_before_running_a_guest(self):
        bad = ((("../invalid-object", b"corrupt"), "invalid persisted"),
               (("Lib/digest", b"alias"), "invalid persisted"),
               (("x" * 1025, b"oversized name"), "invalid persisted"),
               (("text", "not a blob"), "is not binary"))
        for row, reason in bad:
            with self.subTest(name=row[0][:20]), tempfile.TemporaryDirectory() as directory:
                with closing(coven_http.database(directory, create=True)) as db:
                    db.executemany("INSERT INTO objects VALUES (?, ?)", [("lib/index", b"keep"), row])
                    db.commit()
                    with self.assertRaisesRegex(ValueError, reason):
                        coven_http.transaction(directory, lambda tree: self.fail("guest reached"))
                    self.assertEqual(dict(db.execute("SELECT name, data FROM objects")), {"lib/index": b"keep", row[0]: row[1]})

    def test_transport_accepts_only_exact_fields_and_a_literal_loopback_endpoint(self):
        self.assertEqual(coven_http.endpoint("http://127.0.0.1:8765/"), 8765)
        for url in ["http://example.org:80", "http://127.0.0.1:8765/?other", "http://127.0.0.1:65536"]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                coven_http.endpoint(url)
        fields = {"signed": b"canonical\x00bytes", "previous": None}
        self.assertEqual(coven_http.decode(coven_http.encode(fields), fields), fields)
        for body in [b'{"signed":null,"signed":null}', b'{"other":"AA=="}', b'{"signed":"!"}']:
            with self.subTest(body=body), self.assertRaises(ValueError):
                coven_http.decode(body, ["signed"])


class HttpServerBoundaries(unittest.TestCase):
    def start(self, request_seconds=1):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.store = Path(temp.name) / "store"
        command = [sys.executable, "-c", FAKE_SERVER, str(HERE), str(self.store), str(request_seconds)]
        self.url = start_server(self, command, Path(temp.name) / "log")
        self.port = coven_http.endpoint(self.url)
        self.host = f"Host: 127.0.0.1:{self.port}\r\n".encode()

    def test_refusals_name_no_server_path(self):
        self.start()
        with self.assertRaises(ValueError) as refused:
            client_request(self.url, "packages/lib/1.0.0", ["digest"], {"field": b"x"})
        text = str(refused.exception)
        self.assertRegex(text, r"refused <staging>/coven-transaction-[^/ ]+/lib/index under <store>$")
        for root in (str(self.store), str(self.store.resolve()), tempfile.gettempdir(), "/private/"):
            self.assertNotIn(root, text)

    def test_exactly_one_host_header(self):
        self.start()
        ok = raw_exchange(self.port, b"GET /v1/packages HTTP/1.0\r\n" + self.host + b"\r\n")
        self.assertTrue(ok.startswith(b"HTTP/1.0 200"), ok)
        for extra in (self.host, b"Host: attacker.example\r\n"):
            reply = raw_exchange(self.port, b"GET /v1/packages HTTP/1.0\r\n" + self.host + extra + b"\r\n")
            self.assertTrue(reply.startswith(b"HTTP/1.0 400"), reply)

    def test_rejects_ambiguous_publication_heads_before_dispatch(self):
        self.start(request_seconds=3)
        body = coven_http.encode({"field": b"x"})
        exact = f"Content-Length: {len(body)}\r\n".encode()
        good = b"POST /v1/packages/lib/1.0.0 HTTP/1.0\r\n" + self.host
        content_type = b"Content-Type: Application/JSON; charset=utf-8\r\n"
        for extra in (exact, b"Transfer-Encoding: chunked\r\n", b"Origin: " + self.url.encode() + b"\r\n",
                      b" Expect: 100-continue\r\n", b"Content-Type: application/json\r\n"):
            with self.subTest(extra=extra):
                refused = raw_exchange(self.port, good + content_type + exact + extra + b"\r\n" + body)
                self.assertTrue(refused.startswith(b"HTTP/1.0 400"), refused)
                self.assertNotIn(b"refused <staging>", refused)
        # The client keeps its write half open. Intake must stop at Content-Length.
        limit = coven_http.LIMIT
        padded = body.ljust(limit, b"\n")
        exact_limit = f"Content-Length: {limit}\r\n".encode()
        valid = raw_exchange(self.port, good + content_type + exact_limit + b"\r\n" + padded)
        self.assertIn(b"refused <staging>", valid)

    def test_writes_a_full_registry_reply_with_exact_framing(self):
        self.start()
        # This field's JSON quotes and delimiters take exactly 12 bytes.
        block = bytes(range(256))
        size = (coven_http.LIMIT - 12) // 4 * 3
        data = (block * (size // len(block) + 1))[:size]
        with closing(coven_http.database(self.store)) as db:
            db.execute("INSERT INTO objects VALUES (?, ?)", ("fixture-reply", data))
            db.commit()
        with closing(HTTPConnection("127.0.0.1", self.port, timeout=10)) as client:
            client.request("GET", "/v1/packages")
            response = client.getresponse()
            self.assertEqual((response.version, response.status), (10, 200))
            self.assertEqual(response.headers.get_all("Content-Length"), [str(coven_http.LIMIT)])
            self.assertEqual(response.headers.get_all("Content-Type"), ["application/json"])
            self.assertEqual(response.headers.get_all("Connection"), ["close"])
            self.assertEqual(coven_http.decode(response.read(), ["bytes"]), {"bytes": data})

    def test_deeply_nested_json_is_a_client_error(self):
        self.start()
        body = b"[" * 200000
        head = (f"POST /v1/packages/lib/1.0.0 HTTP/1.0\r\nContent-Type: application/json\r\n"
                f"Content-Length: {len(body)}\r\n").encode() + self.host + b"\r\n"
        reply = raw_exchange(self.port, head + body)
        self.assertTrue(reply.startswith(b"HTTP/1.0 400"), reply[:200])
        with self.assertRaisesRegex(ValueError, "flat JSON object"):
            coven_http.decode(b'{"field":' * 100000, ["field"])

    def test_a_slow_drip_cannot_hold_the_server_past_one_deadline(self):
        self.start(request_seconds=0.35)
        # Check for closure between sends while the request is still incomplete.
        # An idle interval after the final send could let a per-read timeout pass.
        stop = time.monotonic() + 1.2
        closed = False
        with socket.create_connection(("127.0.0.1", self.port), timeout=10) as dripper:
            for byte in b"GET /v1/packages HTTP/1.0\r\nX-Slow: " + b"a" * 40:
                if time.monotonic() >= stop:
                    break
                try:
                    dripper.sendall(bytes([byte]))
                    readable, _, _ = select.select([dripper], [], [], min(0.04, max(0, stop - time.monotonic())))
                    if readable:
                        closed = dripper.recv(1024) == b""
                        break
                except (BrokenPipeError, ConnectionResetError):
                    closed = True
                    break
        self.assertTrue(closed, "the server still waits on a dripping request")
        self.assertTrue(raw_exchange(self.port, b"GET /v1/packages HTTP/1.0\r\n" + self.host + b"\r\n").startswith(b"HTTP/1.0 200"))


class HttpClientFraming(unittest.TestCase):
    def exchange(self, respond, check):
        with socket.create_server(("127.0.0.1", 0)) as server, ThreadPoolExecutor(max_workers=1) as pool:
            server.settimeout(10)
            endpoint = f"127.0.0.1:{server.getsockname()[1]}"

            def peer():
                connection, _ = server.accept()
                with connection, connection.makefile("rb") as stream:
                    connection.settimeout(3)
                    self.assertEqual(stream.readline(), b"GET /v1/snapshot HTTP/1.1\r\n")
                    self.assertEqual(stream.readline(), f"Host: {endpoint}\r\n".encode())
                    while True:
                        line = stream.readline()
                        if line == b"\r\n":
                            break
                        self.assertTrue(line, "client closed before sending the complete head")
                    respond(connection)

            worker = pool.submit(peer)
            check(f"http://{endpoint}")
            worker.result(timeout=10)

    def test_streams_chunked_objects_and_refuses_partial_responses(self):
        content = bytes(range(256)) * 128
        body = coven_http.encode({"snapshot": content})

        def chunked(connection):
            connection.sendall(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            for start in range(0, len(body), 4093):
                data = body[start:start + 4093]
                connection.sendall(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            connection.sendall(b"0\r\n\r\n")

        def check(url):
            self.assertEqual(client_request(url, "snapshot", ["snapshot"]), {"snapshot": content})

        self.exchange(chunked, check)
        for length, message in ((len(body) + 1, "incomplete Coven response"),
                                (coven_http.LIMIT + 1, "response exceeds the transport limit")):
            with self.subTest(length=length):
                def truncated(connection):
                    head = f"HTTP/1.0 200 OK\r\nContent-Length: {length}\r\n\r\n"
                    connection.sendall(head.encode() + body[:40])

                def refused(url):
                    with self.assertRaisesRegex(ValueError, message):
                        client_request(url, "snapshot", ["snapshot"])

                self.exchange(truncated, refused)

    def test_client_deadline_covers_continuous_response_progress(self):
        def respond(connection):
            connection.sendall(b"HTTP/1.0 200 OK\r\nContent-Length: 30\r\n\r\n")
            sent = 0
            for _ in range(30):
                try:
                    connection.sendall(b"x")
                    sent += 1
                except (BrokenPipeError, ConnectionResetError):
                    break
                time.sleep(0.05)
            self.assertGreater(sent, 1)
            self.assertLess(sent, 30)

        def refused(url):
            with patch.object(coven_http, "EXCHANGE_MS", 500):
                with self.assertRaisesRegex(ValueError, "exchange deadline exceeded"):
                    client_request(url, "snapshot", ["snapshot"])

        self.exchange(respond, refused)


class HttpClientText(unittest.TestCase):
    def test_server_text_cannot_reach_the_terminal_raw(self):
        class Hostile(BaseHTTPRequestHandler):
            def do_GET(self):
                body = b"\x1b[2J\x1b]0;owned\x07refused\ncoven: lib 1.0.0: verified\xe2\x80\xae"
                self.send_response(400)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass
        server = HTTPServer(("127.0.0.1", 0), Hostile)
        worker = threading.Thread(target=server.handle_request, daemon=True)
        worker.start()
        self.addCleanup(server.server_close)
        self.addCleanup(worker.join, 2)
        with self.assertRaises(ValueError) as refused:
            client_request(f"http://127.0.0.1:{server.server_port}", "snapshot", ["snapshot"])
        text = str(refused.exception)
        self.assertFalse(any(ord(c) < 0x20 or 0x7f <= ord(c) < 0xa0 or c == "‮" for c in text), repr(text))
        self.assertIn("\\x1b[2J", text)


def key(root, name):
    seed = root / name
    seed.write_bytes((HERE / "test-keys" / name).read_bytes())
    seed.chmod(0o600)
    public = (HERE / "test-keys" / f"{name}.pub").read_text()
    Path(f"{seed}.pub").write_text(public)
    return seed, "witchy-2:" + hashlib.sha384(base64.b64decode(public.split()[1])).hexdigest()


class HttpService(unittest.TestCase):
    """The real server and its guests: authors publish and the operator snapshots over HTTP."""
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        previous = os.environ.get("XDG_STATE_HOME")
        os.environ["XDG_STATE_HOME"] = str(self.root / "state")
        self.addCleanup(lambda: os.environ.pop("XDG_STATE_HOME") if previous is None else os.environ.update(XDG_STATE_HOME=previous))
        self.enterContext(policy_signatures())
        self.store = self.root / "store"
        command = [str(COVEN), "serve", "--store", str(self.store), "--port", "0"]
        self.url = start_server(self, protocol_http_server(command), self.root / "server-log")
        self.operator, self.operator_id = key(self.root, "operator")
        first = self.coven("snapshot", "--registry", self.store, "--key", self.operator, "--first", "--id", "test")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.latest = 1

    def coven(self, *args):
        return invoke_coven([COVEN, *map(str, args)], self.root, resident_http=True)

    def package(self, folder, version="1.0.0", name="lib", requires=""):
        package = self.root / folder
        package.mkdir()
        (package / "lib.wy").write_text(f"pub fn one() -> U64:\n    return {len(folder)}\n")
        (package / "witchy.toml").write_text(f'[package]\nname = "{name}"\nversion = "{version}"\nsource = "lib.wy"\n'
                                             f'\n[dependencies]\n{requires}')
        return package

    def publish(self, package, owner):
        return self.coven("publish", package, "--registry", self.url, "--key", owner,
                          "--operator", self.operator_id, "--registry-id", "test")

    def snapshot(self, *admit, registry=None):
        result = self.coven("snapshot", "--registry", registry or self.url, "--key", self.operator, "--id", "test",
                            "--min-base", self.latest, *admit)
        if result.returncode == 0:
            self.latest += 1
        return result

    def catalog(self):
        result = self.coven("catalog", "--registry", self.url, "--operator", self.operator_id,
                            "--registry-id", "test")
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_a_refused_publish_does_not_become_the_owners_next_base(self):
        alice, _ = key(self.root, "alice")
        refused = self.publish(self.package("needs", requires='leaf = "1.0.0"\n'), alice)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("unpublished dependency leaf 1.0.0", refused.stderr)
        published = self.publish(self.package("other", version="1.0.1"), alice)
        self.assertEqual(published.returncode, 0, published.stderr)
        # The refused index's sequence is never signed again.
        self.assertIn("lib: index 2", published.stdout)
        self.assertEqual(self.catalog()["releases"], [])
        local = self.snapshot(registry=self.store)
        self.assertNotEqual(local.returncode, 0)
        self.assertIn("give --admit lib", local.stderr)
        admitted = self.snapshot("--admit", "lib", registry=self.store)
        self.assertEqual(admitted.returncode, 0, admitted.stderr)
        fetched = self.coven("fetch", "lib", "1.0.1", "--registry", self.url, "--into", "out",
                             "--operator", self.operator_id, "--registry-id", "test")
        self.assertEqual(fetched.returncode, 0, fetched.stderr)
        later = self.publish(self.package("third", version="1.0.2"), alice)
        self.assertEqual(later.returncode, 0, later.stderr)
        self.assertIn("lib: index 3", later.stdout)
        catalog = self.catalog()
        self.assertEqual(catalog["sequence"], self.latest)
        self.assertEqual(len(catalog["releases"]), 1)
        self.assertEqual((catalog["releases"][0]["name"], catalog["releases"][0]["version"]),
                         ("lib", "1.0.1"))
        # The next snapshot needs the previous index and rotation chain.
        # Release data is fetched by the consumer, whose checks still apply.
        log = self.root / "server-log"
        offset = log.stat().st_size
        updated = self.snapshot()
        self.assertEqual(updated.returncode, 0, updated.stderr)
        self.assertEqual(self.latest, 3)
        requests = log.read_bytes()[offset:]
        self.assertIn(b"GET /v1/packages/lib/index/2/none ", requests)
        self.assertNotRegex(requests, rb"GET /v1/packages/lib/[0-9]")
        with closing(sqlite3.connect(self.store / "coven-registry.sqlite3")) as db, db:
            db.execute("UPDATE objects SET data = ? WHERE name = ?", (b"altered", "lib/indexes/3"))
        refused = self.coven("catalog", "--registry", self.url, "--operator", self.operator_id,
                             "--registry-id", "test")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("registry serves another index 3 than its snapshot 3 names", refused.stderr)
        self.assertEqual(refused.stdout, "")


if __name__ == "__main__":
    unittest.main()
