"""Loopback transport and transactional storage for Coven's existing signed objects."""
import base64
import contextlib
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import stat
import subprocess
import tempfile
import time

from coven_staging import read_file
from coven_worker import run_with_lifeline, worker_module

LIMIT = 16 << 20
EXCHANGE_MS = 60000
CLIENT_BUILD = ("--heap-pages=4", "--slots=64", "--stack-bytes=65536")
CLIENT_MODULE_BYTES = 131072
CLIENT_STATUS = re.compile(rb"status ([2-5][0-9]{2})\n")
CLIENT_REFUSALS = {
    b"refused 1\n": "invalid Coven HTTP request",
    b"refused 2\n": "Coven HTTP connection failed",
    b"refused 3\n": "incomplete Coven response or invalid framing",
    b"refused 4\n": "Coven HTTP response could not be staged",
    b"refused 5\n": "registry response exceeds the transport limit",
}
ADMISSION_BUILD = ("--heap-pages=4", "--slots=64", "--stack-bytes=32768")
ADMISSION_MODULE_BYTES = 131072
REPLY_BUILD = ("--heap-pages=4", "--slots=64", "--stack-bytes=32768")
REPLY_MODULE_BYTES = 131072
REPLY_HEAD_BYTES = 2048
HEAD_BYTES = 16384
ADMITTED = re.compile(rb"([GPW])\n([01])\n(0|[1-9][0-9]{0,8})\n([\x21-\x7e]+)\ncomplete\n")
ADMISSION_REFUSALS = {
    b"refused 400\n": (400, b"invalid registry request"),
    b"refused 404\n": (400, b"unknown registry route"),
    b"refused 413\n": (400, b"invalid publication size"),
    b"refused 421\n": (400, b"use the local registry origin directly"),
    b"refused 431\n": (431, b"registry request head exceeds its size limit"),
    b"refused 501\n": (501, b"unsupported registry method"),
    b"refused 503\n": (503, b"registry storage or toolchain unavailable"),
}
DATABASE = "coven-registry.sqlite3"
OBJECT_NAME_BYTES = 1024
OBJECT_CHUNK = 65536
OBJECT_PART = re.compile(r"[a-z0-9._-]+")
# One deadline for a whole request (request line, headers and body) and one for sending the
# reply: a client that drips bytes cannot hold the one-at-a-time server past them.
REQUEST_SECONDS = 30
REPLY_SECONDS = 30
# Every valid message is one flat JSON object of strings and nulls.
JSON_STRING = re.compile(rb'"[^"\\]*(?:\\.[^"\\]*)*"')
CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f\u2028\u2029\u200e\u200f\u202a-\u202e\u2066-\u2069]")
ABSOLUTE_PATH = re.compile(r"(?:^|(?<=[\s'\"(=:,]))/[^\s'\"]*")


def printable(text):
    """Server-supplied text for a terminal: control and bidirectional-formatting characters
    escaped, so a server cannot move the cursor, recolor or rewrite what the user reads."""
    return CONTROL.sub(lambda match: f"\\x{ord(match[0]):02x}" if ord(match[0]) < 0x100 else f"\\u{ord(match[0]):04x}", text)


def endpoint(url):
    match = re.fullmatch(r"http://127\.0\.0\.1:([1-9][0-9]{0,4})/?", str(url))
    if not match or int(match[1]) > 65535:
        raise ValueError("registry URL must be http://127.0.0.1:PORT")
    return int(match[1])


def encode(objects):
    return json.dumps({key: base64.b64encode(value).decode("ascii") if value is not None else None
                       for key, value in objects.items()}, separators=(",", ":")).encode("ascii")


def decode(raw, fields):
    def unique(pairs):
        result = dict(pairs)
        if len(result) != len(pairs):
            raise ValueError("duplicate registry field")
        return result
    # Refuse nesting before parsing: a deep document would exhaust the parser's recursion.
    outline = JSON_STRING.sub(b'""', raw if isinstance(raw, bytes) else raw.encode())
    if outline.count(b"{") > 1 or b"[" in outline:
        raise ValueError("registry message must be one flat JSON object")
    try:
        value = json.loads(raw, object_pairs_hook=unique)
    except RecursionError:
        raise ValueError("registry message nested too deeply") from None
    if not isinstance(value, dict) or value.keys() != set(fields):
        raise ValueError("unexpected registry fields")
    result = {}
    for key, item in value.items():
        if item is not None and not isinstance(item, str):
            raise ValueError("invalid registry field")
        result[key] = None if item is None else base64.b64decode(item, validate=True)
    return result


@contextlib.contextmanager
def request_worker(url, build, host):
    """Hold one checked client module for all exchanges in a command to one origin."""
    authority = f"127.0.0.1:{endpoint(url)}"
    with worker_module(build, "legacy_http_request", CLIENT_BUILD, CLIENT_MODULE_BYTES,
                       "legacy loopback HTTP") as module:
        def exchange(route, fields, upload=None):
            return request(authority, route, fields, upload, module=module, host=host)

        yield exchange


def request(authority, route, fields, upload, *, module, host):
    body = encode(upload) if upload is not None else b""
    if len(body) > LIMIT:
        raise ValueError("registry upload exceeds the transport limit")
    mode = "GET" if upload is None else str(len(body))
    header = f"{authority}\n/v1/{route}\n{mode}\n".encode("ascii")
    with tempfile.TemporaryDirectory(prefix="coven-loopback-") as directory:
        root = Path(directory).resolve()
        source, response = root / "upload", root / "reply"
        source.write_bytes(body)
        try:
            command = (host, str(module), f"--file={source}", f"--write-file={response}",
                       f"--net={authority}", "--no-links", "--input-stdin", "--output", "--allocator",
                       "--input-bytes=512", "--output-bytes=32", "--memory-pages=16",
                       f"--fs-bytes={len(body) + LIMIT + 512}", "--net-bytes=67108864",
                       f"--module-bytes={CLIENT_MODULE_BYTES}", "--fuel=300000000",
                       f"--deadline-ms={EXCHANGE_MS}")
            result = run_with_lifeline(subprocess.run, command, input=header, capture_output=True,
                                       timeout=EXCHANGE_MS / 1000 + 2)
        except subprocess.TimeoutExpired:
            raise ValueError("Coven HTTP exchange deadline exceeded") from None
        if result.returncode == 124 and b"limit reached: deadline:" in result.stderr:
            raise ValueError("Coven HTTP exchange deadline exceeded")
        if result.returncode:
            raise ValueError("Coven HTTP exchange stopped")
        status = CLIENT_STATUS.fullmatch(result.stdout)
        if status is None:
            raise ValueError(CLIENT_REFUSALS.get(result.stdout, "invalid Coven HTTP worker result"))
        data = read_file(response, LIMIT)
    code = int(status[1])
    if code != 200:
        raise ValueError(f"Coven HTTP {code}: {printable(data[:500].decode(errors='replace'))}")
    return decode(data, fields)


def database(store, create=False):
    store = Path(store).absolute()
    if create:
        store.mkdir(mode=0o700, parents=True, exist_ok=True)
    mode = store.lstat()
    if not stat.S_ISDIR(mode.st_mode) or mode.st_uid != os.getuid() or mode.st_mode & 0o077:
        raise ValueError("Coven storage must be an owner-only directory (mode 0700)")
    db = store / DATABASE
    if os.path.lexists(db):
        status = db.lstat()
        if not stat.S_ISREG(status.st_mode) or status.st_uid != os.getuid() or status.st_nlink != 1:
            raise ValueError("refusing non-private registry database file")
    elif create and any(store.iterdir()):
        raise ValueError("initialize Coven in an empty private directory")
    if not create and not db.is_file():
        raise ValueError("missing registry database")
    connection = sqlite3.connect(db, timeout=60)
    if create:
        connection.execute("CREATE TABLE IF NOT EXISTS objects (name TEXT PRIMARY KEY, data BLOB NOT NULL)")
        connection.commit()
    return connection


def object_parts(name):
    if not isinstance(name, str) or len(name) > OBJECT_NAME_BYTES:
        raise ValueError("invalid persisted registry object name")
    parts = name.split("/")
    if any(part in (".", "..") or not OBJECT_PART.fullmatch(part) for part in parts):
        raise ValueError("invalid persisted registry object name")
    return parts


def copy_object(source, destination):
    digest = hashlib.sha384()
    count = 0
    while chunk := source.read(OBJECT_CHUNK):
        digest.update(chunk)
        destination.write(chunk)
        count += len(chunk)
    return count, digest.digest()


OBJECT_ROWS = (
    "SELECT rowid, CASE WHEN typeof(name) = 'text' AND length(CAST(name AS BLOB)) <= ? "
    "THEN name END, typeof(data) FROM objects"
)


class PublicationScope:
    def __init__(self, package, version, dependencies):
        self.package = package
        self.package_prefix = package + "/"
        slot = self.package_prefix + version + "/"
        self.outputs = {slot + name for name in ("package", "source.wy", "digest", "release-signature")}
        self.outputs.add(self.package_prefix + "index")
        self.read_only = {"snapshot"}
        for dependency, exact in dependencies.items():
            coordinate = dependency + "/" + exact
            self.read_only.update((dependency, coordinate, coordinate + "/digest"))

    def includes_package(self, name):
        return name == self.package or name.startswith(self.package_prefix)

    def may_write(self, name):
        return name in self.outputs


def stage_objects(db, tree, scope=None):
    staged = {}
    if scope is None:
        stage_rows(db, tree, staged, OBJECT_ROWS, ())
    else:
        # The upper bound is the same prefix with its trailing slash advanced
        # to '0'. The name column uses binary ordering.
        stage_rows(db, tree, staged, OBJECT_ROWS + " WHERE name = ? OR (name >= ? AND name < ?)",
                   (scope.package, scope.package_prefix, scope.package + "0"))
        for name in sorted(scope.read_only):
            if not scope.includes_package(name):
                stage_rows(db, tree, staged, OBJECT_ROWS + " WHERE name = ?", (name,))
    return staged


def stage_rows(db, tree, staged, query, parameters):
    with contextlib.closing(db.execute(query, (OBJECT_NAME_BYTES, *parameters))) as rows:
        for row, name, kind in rows:
            path = tree.joinpath(*object_parts(name))
            if kind != "blob":
                raise ValueError("persisted registry object is not binary")
            path.parent.mkdir(parents=True, exist_ok=True)
            with db.blobopen("objects", "data", row, readonly=True) as source, path.open("xb") as output:
                staged[name] = copy_object(source, output)


def reject_scan_error(error):
    raise error


def store_objects(db, tree, staged, scope=None):
    for directory, folders, files in os.walk(tree, onerror=reject_scan_error):
        for folder in folders:
            if (Path(directory) / folder).is_symlink():
                raise ValueError("unexpected link in registry transaction")
        for filename in files:
            path = Path(directory) / filename
            name = path.relative_to(tree).as_posix()
            object_parts(name)
            if not stat.S_ISREG(path.lstat().st_mode):
                raise ValueError("unexpected non-private file in registry transaction")
            flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
            fd = os.open(path, flags)
            with os.fdopen(fd, "rb") as source:
                info = os.fstat(source.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                    raise ValueError("unexpected non-private file in registry transaction")
                signature = (info.st_size, hashlib.file_digest(source, "sha384").digest())
                if signature != staged.pop(name, None):
                    if scope is not None and not scope.may_write(name):
                        raise ValueError("publication changed an object outside its slot and index")
                    store_object(db, name, source, signature)
    if scope is not None and staged:
        raise ValueError("publication removed a registry object")
    db.executemany("DELETE FROM objects WHERE name=?", ((name,) for name in staged))


def store_object(db, name, source, signature):
    with contextlib.closing(db.execute(
        "INSERT INTO objects VALUES(?, zeroblob(?)) "
        "ON CONFLICT(name) DO UPDATE SET data = excluded.data RETURNING rowid",
        (name, signature[0]),
    )) as statement:
        row, = statement.fetchone()
    source.seek(0)
    with db.blobopen("objects", "data", row) as destination:
        if copy_object(source, destination) != signature:
            raise ValueError("registry transaction object changed during commit")


def transaction(store, operation, *, scope=None):
    """Existing file-capability guests operate on a private transaction. Commit only on success."""
    with contextlib.closing(database(store)) as db, db, tempfile.TemporaryDirectory(prefix="coven-transaction-") as tmp:
        db.execute("BEGIN IMMEDIATE")
        tree = Path(tmp).resolve()
        staged = stage_objects(db, tree, scope)
        value = operation(tree)
        store_objects(db, tree, staged, scope)
        return value


def read_public_objects(db, names):
    """Read one response's objects within its raw byte budget and one read transaction."""
    result = {}
    remaining = LIMIT
    for key, name in names.items():
        # Length of a BLOB is in bytes. Leave an oversized value in SQLite.
        row = db.execute(
            "SELECT typeof(data), CASE WHEN typeof(data) = 'blob' AND length(data) <= ? THEN data END "
            "FROM objects WHERE name = ?", (remaining, name),
        ).fetchone()
        if row is None:
            result[key] = None
            continue
        kind, data = row
        if kind != "blob":
            raise TypeError("persisted registry object is not binary")
        if data is None:
            raise ValueError("registry response exceeds the transport limit")
        remaining -= len(data)
        result[key] = data
    return result


def redacted(text, roots):
    """A refusal for the client: the server's directories and any other absolute path removed."""
    for root, label in roots:
        text = text.replace(root, label)
    return printable(ABSOLUTE_PATH.sub("<path>", text))


class RequestRefused(ValueError):
    def __init__(self, status, diagnostic):
        super().__init__(diagnostic.decode("ascii"))
        self.status = status
        self.diagnostic = diagnostic


BODY_PHASE = b"body\n"


def request_timeout(output):
    if output and output.startswith(BODY_PHASE):
        raise RequestRefused(408, b"request deadline exceeded")
    raise TimeoutError("request deadline exceeded")


def intake_request(host, module, connection, authority, web, until):
    with tempfile.TemporaryDirectory(prefix="coven-intake-") as directory:
        publication = Path(directory).resolve() / "publication"
        wire = run_intake(host, module, connection, authority, web, publication, until)
        if not wire:
            return None
        body_phase = wire.startswith(BODY_PHASE)
        verdict = wire.removeprefix(BODY_PHASE)
        if body_phase and verdict == b"refused 400\n":
            raise RequestRefused(400, b"incomplete publication")
        if refusal := ADMISSION_REFUSALS.get(verdict):
            raise RequestRefused(*refusal)
        parsed = ADMITTED.fullmatch(verdict)
        if parsed is None:
            raise OSError("invalid Witchy registry admission result")
        mode, minor, length, target = parsed.groups()
        size = int(length)
        if size > LIMIT or body_phase != (size != 0) or (mode != b"P" and size != 0):
            raise OSError("invalid Witchy registry admission length")
        body = read_file(publication, size) if mode == b"P" else b""
        if len(body) != size:
            raise OSError("incomplete staged publication")
    return mode, target.decode("ascii"), body, minor.decode("ascii")


def run_intake(host, module, connection, authority, web, publication, until):
    remaining = until - time.monotonic()
    if remaining <= 0:
        request_timeout(None)
    duration = max(1, int(remaining * 1000))
    mode = "web" if web else "registry"
    # The full body limit fits even when every byte ends its Console input run.
    command = (host, str(module), "--allocator", "--input-stdin", "--output", "--stream-output",
               "--args=2:32", f"--arg={authority}", f"--arg={mode}",
               f"--write-file={publication}", "--no-links", f"--fs-bytes={LIMIT + 256}",
               f"--input-bytes={HEAD_BYTES + LIMIT}", f"--output-bytes={HEAD_BYTES + 32}",
               "--memory-pages=12", f"--module-bytes={ADMISSION_MODULE_BYTES}",
               "--fuel=6000000000", f"--deadline-ms={duration}")
    try:
        result = run_connection_worker(command, remaining, stdin=connection, stdout=subprocess.PIPE)
    except subprocess.TimeoutExpired as error:
        request_timeout(error.output)
    if time.monotonic() >= until or (result.returncode == 124 and b"limit reached: deadline:" in result.stderr):
        request_timeout(result.stdout)
    if result.returncode != 0:
        if b"limit reached: input bytes:" in result.stderr:
            raise RequestRefused(400, b"registry request exceeds its size limit")
        raise OSError("Witchy registry request intake stopped")
    return result.stdout


def run_connection_worker(command, remaining, *, stdin, stdout):
    return run_with_lifeline(subprocess.run, command, stdin=stdin, stdout=stdout,
                             stderr=subprocess.PIPE, timeout=remaining)


def send_reply(host, module, connection, status, body, content_type, policy):
    length = len(body)
    if length > LIMIT:
        raise ValueError("registry response exceeds the transport limit")
    until = time.monotonic() + REPLY_SECONDS
    with tempfile.TemporaryDirectory(prefix="coven-reply-") as directory:
        source = Path(directory).resolve() / "reply"
        source.write_bytes(body)
        remaining = until - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("reply deadline exceeded")
        duration = max(1, int(remaining * 1000))
        command = (host, str(module), "--allocator", "--output", "--args=4:1024",
                   f"--arg={status}", f"--arg={length}", f"--arg={content_type}", f"--arg={policy or ''}",
                   f"--file={source}", "--no-links", f"--fs-bytes={length}",
                   f"--output-bytes={length + REPLY_HEAD_BYTES}", "--memory-pages=8",
                   f"--module-bytes={REPLY_MODULE_BYTES}", "--fuel=500000000", f"--deadline-ms={duration}")
        try:
            result = run_connection_worker(command, remaining, stdin=subprocess.DEVNULL, stdout=connection)
        except subprocess.TimeoutExpired:
            raise TimeoutError("reply deadline exceeded") from None
        if result.returncode == 124:
            raise TimeoutError("reply deadline exceeded")
        if result.returncode != 0:
            raise OSError("Witchy registry reply stopped")


def serve(store, port, routes, accept, errors, *, build, host, web=None, web_default=None):
    database(store, create=True).close()
    # Longest first, so a staging directory inside the store is named as staging.
    roots = sorted({(str(path), "<store>") for path in (Path(store).absolute(), Path(store).resolve())}
                   | {(str(path), "<staging>") for path in (Path(tempfile.gettempdir()).absolute(), Path(tempfile.gettempdir()).resolve())},
                   key=lambda pair: len(pair[0]), reverse=True)

    class RegistryHandler(BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.rfile.close()
            self.request_deadline = time.monotonic() + REQUEST_SECONDS

        def respond(self, status, body, content_type, policy=None):
            self.log_request(status)
            policy = web_default if policy is None else policy
            try:
                send_reply(host, reply_module, self.connection, status, body, content_type, policy)
            except (OSError, ValueError) as error:
                self.log_error("reply stopped: %s", error)

        def handle_one_request(self):
            self.close_connection = True
            self.request_version = "HTTP/1.0"
            self.requestline = ""
            try:
                authority = f"127.0.0.1:{self.server.server_port}"
                decision = intake_request(host, module, self.connection, authority,
                                          web is not None, self.request_deadline)
                if decision is None:
                    return
            except RequestRefused as refused:
                self.respond(refused.status, refused.diagnostic, "text/plain")
                return
            except TimeoutError:
                self.log_error("request deadline exceeded")
                return
            except OSError as error:
                self.log_error("unavailable: %r", error)
                self.respond(503, b"registry storage or toolchain unavailable", "text/plain")
                return
            mode, target, body, minor = decision
            method = "POST" if mode == b"P" else "GET"
            self.requestline = f"{method} {target} HTTP/1.{minor}"
            self.dispatch(mode, target, body)

        def dispatch(self, mode, target, body):
            if mode == b"W":
                resource = web.get(target)
                if resource is None:
                    self.respond(404, b"unknown Coven Web route", "text/plain")
                else:
                    body, content_type, policy = resource
                    self.respond(200, body, content_type, policy)
                return
            route = target[4:].split("/")
            try:
                if mode == b"G":
                    with contextlib.closing(database(store)) as db:
                        db.execute("BEGIN")
                        if route == ["catalog"]:
                            # A coordinate per package lets an operator review the published heads.
                            packages = {}
                            for (name,) in db.execute("SELECT name FROM objects WHERE name LIKE '%/digest' ORDER BY name"):
                                parts = name.split("/")
                                if len(parts) == 3:
                                    packages.setdefault(parts[0], parts[1])
                            result = {"packages": json.dumps(packages).encode()}
                        else:
                            result = read_public_objects(db, routes(route))
                else:
                    result = accept(store, route, body)
                body = encode(result)
                if len(body) > LIMIT:
                    raise ValueError("registry response exceeds the transport limit")
            except errors as error:
                # Details, paths included, stay in the server's log; the client gets the reason.
                self.log_error("refused: %r", str(error))
                self.respond(400, redacted(str(error), roots).encode()[:1024], "text/plain")
                return
            except RecursionError:
                self.respond(400, b"invalid registry message", "text/plain")
                return
            except TimeoutError:
                self.close_connection = True
                self.respond(408, b"request deadline exceeded", "text/plain")
                return
            except (sqlite3.Error, OSError) as error:
                self.log_error("unavailable: %r", error)
                self.respond(503, b"registry storage or toolchain unavailable", "text/plain")
                return
            except Exception as error:
                self.log_error("internal error: %r", error)
                self.respond(500, b"internal registry error", "text/plain")
                return
            self.respond(200, body, "application/json")

    with (worker_module(build, "legacy_http_admit", ADMISSION_BUILD, ADMISSION_MODULE_BYTES,
                        "legacy HTTP request admission") as module,
          worker_module(build, "legacy_http_reply", REPLY_BUILD, REPLY_MODULE_BYTES,
                        "legacy HTTP response") as reply_module,
          HTTPServer(("127.0.0.1", port), RegistryHandler) as server):
        print(f"Coven listening at http://127.0.0.1:{server.server_port}", flush=True)
        def shutdown(signum, frame):
            raise KeyboardInterrupt
        previous = signal.signal(signal.SIGTERM, shutdown)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            signal.signal(signal.SIGTERM, previous)
