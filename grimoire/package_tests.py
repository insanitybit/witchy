"""Package test supervisor. Public requests name declarations and public inputs only."""

import contextlib
import fnmatch
import hashlib
import io
import json
import os
from pathlib import Path
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import time
import tomllib

# The launcher injects its verified test_sources and property_trials companions.
# Type-only imports keep the source navigable without importing from a package's cwd.
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import test_sources
    import property_trials

MAX_INPUT = 65536
MAX_BATCH = 64
WORD = struct.Struct(">Q")
HEAD = struct.Struct(">BBQI")
DIR_TYPES = {"&Dir<Read>": 0, "&Dir<Write>": 1}
# The external wire contract. Nominal fixture values are constructed inside Witchy.
PROVIDERS = {
    "U64": {"word"},
    "&Console<Read>": {"bytes"},
    "&Dir<Read>": {"fixture", "read_dir", "subdir"},
    "&Dir<Write>": {"fixture", "write_dir", "subdir"},
    # A heap of the case's own, from the test host: no provider is named for it, or accepted.
    "&Allocator": set(),
    "Allocator": set(),
}
HEAPS = ("&Allocator", "Allocator")
# How long one case may run, in milliseconds, unless the manifest or the command says.
CASE_DEADLINE_MS = 100


def options(cli):
    cli.add_argument(
        "--projects", action="store_true", help="run legacy [[test]] projects"
    )
    mode = cli.add_mutually_exclusive_group()
    mode.add_argument(
        "--session",
        action="store_true",
        help="serve version 1 list/run on stdin/stdout",
    )
    mode.add_argument("--list", action="store_true")
    which = cli.add_mutually_exclusive_group()
    which.add_argument("--case", help="exact package-relative case ID")
    which.add_argument("--filter", help="case-sensitive glob")
    which.add_argument(
        "--property", metavar="CASE", help="run deterministic public U64 trials"
    )
    which.add_argument(
        "--replay", type=Path, help="re-execute a minimized exact replay"
    )
    cli.add_argument("--seed", type=int, default=0)
    cli.add_argument("--trials", type=int, default=32)
    cli.add_argument("--save-replay", type=Path)
    mode.add_argument(
        "--reject",
        type=Path,
        metavar="tests/reject/FILE.wy",
        help="check an ordinary consumer refusal",
    )
    cli.add_argument(
        "--expect",
        metavar="CODE@LINE:COLUMN",
        help="exact primary diagnostic in the rejection entry",
    )
    cli.add_argument(
        "--lemma", metavar="NAME", help="require this kernel-refused lemma"
    )
    mode.add_argument(
        "--proof",
        metavar="DECLARATION",
        help="select a checked proof obligation without running guests",
    )
    cli.add_argument(
        "--dependency", metavar="NAME", help="select an authenticated dependency suite"
    )
    cli.add_argument("--artifact", help="require this artifact identity")
    cli.add_argument("--json", action="store_true")
    for flag in ("read-dir", "write-dir", "fixture", "bytes", "word"):
        cli.add_argument(
            "--" + flag, action="append", default=[], metavar="CASE:SLOT=VALUE"
        )
    for flag in ("read-root", "write-root"):
        cli.add_argument(
            "--" + flag, action="append", default=[], metavar="NAME=/absolute/directory"
        )
    cli.add_argument(
        "--coverage", action="store_true", help="collect guest execution feedback"
    )
    cli.add_argument(
        "--case-deadline-ms",
        type=int,
        metavar="N",
        help="how long one case may run, 25 to 4000 (default: [tests] case-deadline-ms, or 100)",
    )


def digest(data):
    return hashlib.sha256(data).hexdigest()


def frame(data):
    return struct.pack(">I", len(data)) + data


def hexadecimal(value):
    if (
        not isinstance(value, str)
        or len(value) > MAX_INPUT * 2
        or not re.fullmatch("(?:[a-fA-F0-9]{2})*", value)
    ):
        raise ValueError("fixture input needs bounded whole hex bytes")
    return bytes.fromhex(value)


def unsigned(value):
    if (
        not isinstance(value, str)
        or not re.fullmatch("0|[1-9][0-9]{0,19}", value)
        or int(value) >= 1 << 64
    ):
        raise ValueError("U64 requires an unsigned canonical decimal string")
    return int(value)


def read_exact(fd, size, deadline):
    data = bytearray()
    while len(data) < size:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
            raise TimeoutError("test host deadline")
        got = os.read(fd, size - len(data))
        if not got:
            raise ValueError("test host disconnected")
        data.extend(got)
    return bytes(data)


class Host:
    def __init__(self, executable, env, root, modules, case_deadline_ms=CASE_DEADLINE_MS):
        self.command = [executable, "--test-session"]
        self.case_deadline_ms = case_deadline_ms
        if case_deadline_ms != CASE_DEADLINE_MS:
            self.command.append(f"--case-deadline-ms={case_deadline_ms}")
        for index, module in enumerate(modules):
            path = root / f"{index}.wasm"
            actual = digest(path.read_bytes())
            if (
                actual != module["sha256"]
                or not module["receipt"]["all_selected_checked"]
                or module["receipt"].get("wasm_sha256", actual) != actual
            ):
                raise ValueError("compiler receipt and test module disagree")
            self.command.extend(
                (
                    path,
                    actual,
                    module["index"],
                    str(len(module["receipt"].get("guest_edges_v1", []))),
                )
            )
        self.root = root
        self.env = env
        self.process = None
        self.compiles = self.stores = 0

    def write(self, data):
        data = memoryview(data)
        while data:
            written = self.process.stdin.write(data)
            if not written:
                raise OSError("test host write failed")
            data = data[written:]

    def run(self, case, words, fixture, dirs, exchange=None):
        if self.process is None:
            self.process = subprocess.Popen(
                self.command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                cwd=self.root,
                env=self.env,
                bufsize=0,
            )
        data = bytearray((case["group"], len(dirs)))
        for kind, dev, ino, path in dirs:
            data.extend(struct.pack(">BQQ", kind, dev, ino) + frame(os.fsencode(path)))
        data.extend(
            frame(
                bytes([case["number"]])
                + b"".join(WORD.pack(n) for n in words)
                + fixture
            )
        )
        # The host's answer: its start-up and the module's compilation (the first case of a
        # run), then the case within its own deadline.
        deadline = time.monotonic() + 1 + self.case_deadline_ms / 1000
        self.write(data)
        fd = self.process.stdout.fileno()
        first = read_exact(fd, 1, deadline)
        while first == b"\x80":
            size = int.from_bytes(read_exact(fd, 4, deadline), "big")
            if exchange is None or size > 8192:
                raise ValueError("unexpected test service request")
            answer = exchange(read_exact(fd, size, deadline))
            self.write(bytes([answer]))
            first = read_exact(fd, 1, deadline)
        status, compiled, ns, length = HEAD.unpack(
            first + read_exact(fd, HEAD.size - 1, deadline)
        )
        if length > 8192 or status > 3 or compiled > 1:
            raise ValueError("invalid host outcome frame")
        count = int.from_bytes(read_exact(fd, 2, deadline), "big")
        if count > 4096:
            raise ValueError("oversized edge set")
        self.feedback = list(
            struct.unpack(">" + str(count) + "H", read_exact(fd, count * 2, deadline))
        )
        result = read_exact(fd, length, deadline)
        self.compiles += compiled
        self.stores += int(status != 3)
        return status, ns, result

    def abort(self):
        if self.process is not None:
            self.process.kill()
            self.process.wait()
            self.process.stdin.close()
            self.process.stdout.close()
            self.process = None

    def close(self):
        if self.process is not None:
            if sys.exc_info()[0] is not None:
                self.process.kill()
            self.process.stdin.close()
            try:
                if self.process.wait(timeout=0.1) and sys.exc_info()[0] is None:
                    raise ValueError("test host failed")
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
                raise ValueError("test host teardown deadline")
            finally:
                self.process.stdout.close()


def describe_failure(why, actual, expected):
    """A `testing.Failure` as (the reason's name, what the two values say), by the check that
    made it (library/wasm-testing.wy): 1 `eq_u64`, 2 `eq_bool`, 3 `ok`, 4 `eq_i64` (each value
    the number's 64 bits), 5 and 6 `eq_bytes` (a byte; the lengths), 7 `set_up` (the code)."""
    signed = lambda bits: bits - (1 << 64) if bits >> 63 else bits
    truth = lambda bit: "true" if bit else "false"
    if why == 1:
        return "u64_mismatch", f"eq_u64: got {actual}, expected {expected}"
    if why == 2:
        return "bool_mismatch", f"eq_bool: got {truth(actual)}, expected {truth(expected)}"
    if why == 3:
        return "condition", "ok: the condition was false"
    if why == 4:
        return "i64_mismatch", f"eq_i64: got {signed(actual)}, expected {signed(expected)}"
    if why == 5:
        return "byte_mismatch", f"eq_bytes: the byte at index {actual} is not the expected {expected} (0x{expected & 0xff:02x})"
    if why == 6:
        return "length_mismatch", f"eq_bytes: got {actual} bytes, expected {expected}; one is the start of the other"
    if why == 7:
        return "set_up_failed", f"set_up: the step answered Err({actual})"
    return "assertion", f"reason {why}: actual {actual}, expected {expected}"


class Blocked(Exception):
    def __init__(self, reason, slot=None):
        super().__init__(reason)
        self.slot = slot

    def outcome(self):
        return {
            "status": "blocked",
            "entered": False,
            "reason": str(self),
            "slot": self.slot,
        }


class Invocation:
    def __init__(self, case, words, fixture, directories, sensitive, resources):
        self.plan = (case, words, fixture, directories)
        self.sensitive = sensitive
        self.resources = resources

    def run(self, host, sites):
        if self.plan is None:
            raise ValueError("test invocation already consumed")
        plan, self.plan = self.plan, None
        with self.resources:
            status, ns, result = host.run(*plan)
        outcome = {"host_ns": ns, "entered": status != 3}
        if status:
            outcome["status"] = {1: "trap", 2: "limit", 3: "blocked"}[status]
            if status == 3:
                directories = [
                    slot["name"]
                    for slot in plan[0]["slots"]
                    if slot["type"] in DIR_TYPES
                ]
                if len(result) != 1 or result[0] >= len(directories):
                    raise ValueError("invalid directory refusal from test host")
                outcome.update(
                    slot=directories[result[0]], reason="directory identity changed"
                )
        elif result == bytes(8):
            outcome["status"] = "pass"
        elif len(result) == 32:
            why, ordinal, actual, expected = struct.unpack("<QQQQ", result)
            if not why or str(ordinal) not in sites:
                raise ValueError("unknown assertion site or invalid result")
            site = sites[str(ordinal)]
            reason, detail = describe_failure(why, actual, expected)
            outcome.update(
                status="assertion",
                reason=reason,
                site={
                    k: v
                    for k, v in site.items()
                    if not self.sensitive or k in ("line", "column")
                },
            )
            if not self.sensitive:
                outcome.update(actual=str(actual), expected=str(expected), detail=detail)
        else:
            raise ValueError("invalid test result")
        if host.coverage and not self.sensitive:
            outcome["features"] = host.feedback
        return outcome


class TestGrants:
    def __init__(self, args, catalog, check_directory, resources):
        self.providers = {}
        self.check_directory = check_directory
        self.catalog = catalog
        self.resources = resources
        self.roots = {}
        self.open_roots = {}
        self.pinned_directories = {}
        for right, bindings in enumerate((args.read_root, args.write_root)):
            for binding in bindings:
                name, sep, path = binding.partition("=")
                if (
                    not sep
                    or not re.fullmatch(r"[a-zA-Z0-9_-]{1,128}", name)
                    or name in self.roots
                ):
                    raise ValueError(
                        "a named root needs a distinct NAME=/absolute/directory"
                    )
                self.roots[name] = (right, path)
        for flag in ("read_dir", "write_dir", "fixture", "bytes", "word"):
            for request in getattr(args, flag):
                target, sep, value = request.partition("=")
                case, colon, slot = target.rpartition(":")
                if (
                    not sep
                    or not colon
                    or case not in catalog
                    or not slot
                    or (case, slot) in self.providers
                ):
                    raise ValueError(
                        "a grant needs one distinct listed CASE:SLOT=VALUE"
                    )
                self.providers[case, slot] = flag, value

    def pinned(self, table, key, path):
        fd = self.check_directory(str(path), retain=True)
        if key not in table:
            self.resources.callback(os.close, fd)
            table[key] = fd
        else:
            try:
                if not os.path.samestat(os.fstat(table[key]), os.fstat(fd)):
                    raise ValueError("authorized directory was replaced")
            finally:
                os.close(fd)
        return table[key]

    def directory(self, root, relative, right, stack):
        if root not in self.roots or self.roots[root][0] != right:
            raise Blocked("missing directory root or wrong right")
        if relative and any(
            not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", part) or part in (".", "..")
            for part in relative.split("/")
        ):
            raise Blocked("invalid relative directory")
        path = Path(self.roots[root][1])
        fd = self.pinned(self.open_roots, root, self.roots[root][1])
        for part in relative.split("/") if relative else ():
            child = os.open(part, test_sources.DIR, dir_fd=fd)
            stack.callback(os.close, child)
            if os.fstat(child).st_dev != os.fstat(fd).st_dev:
                raise Blocked("directory crosses a mount")
            fd = child
        return path / relative, os.fstat(fd)

    def bind(self, case, inputs, defaults=None):
        start = time.monotonic_ns()
        # Resolve identity and right together. Only this plan reaches the host.
        stack = contextlib.ExitStack()
        try:
            choices = {
                n: v for (key, n), v in self.providers.items() if key == case["id"]
            }
            slots = {s["name"]: s["type"] for s in case["slots"]}
            extra = (
                choices.keys() | inputs.keys() | (defaults or {}).keys()
            ) - slots.keys()
            if extra:
                raise Blocked("unexpected slot", sorted(extra)[0])
            words, dirs, fixture, sensitive = [], [], b"", False
            chosen = {}
            for name, kind in slots.items():
                source = inputs.get(name, choices.get(name, (defaults or {}).get(name)))
                if kind in HEAPS:
                    if source is not None:
                        raise Blocked("wrong provider or right", name)
                    continue
                if source is None:
                    raise Blocked("missing provider", name)
                if source[0] not in PROVIDERS[kind]:
                    raise Blocked("wrong provider or right", name)
                chosen[name] = source
            for name, kind in slots.items():
                if kind in HEAPS:
                    continue
                try:
                    flag, value = chosen[name]
                    if kind == "U64" and flag == "word":
                        words.append(unsigned(value))
                    elif kind == "&Console<Read>" and flag == "bytes":
                        fixture = hexadecimal(value)
                    elif kind in DIR_TYPES:
                        if flag == "fixture":
                            path = Path(
                                stack.enter_context(
                                    tempfile.TemporaryDirectory(
                                        prefix="witchy-fixture-"
                                    )
                                )
                            ).resolve()
                            filename, sep, content = value.partition(":")
                            if not sep or not re.fullmatch(
                                "[a-zA-Z0-9_-]{1,128}", filename
                            ):
                                raise Blocked("invalid fixture", name)
                            (path / filename).write_bytes(hexadecimal(content))
                            handle = test_sources.opened_directory(path)
                            stack.callback(os.close, handle)
                            stat = os.fstat(handle)
                        elif flag == "subdir":
                            try:
                                path, stat = self.directory(
                                    *value, DIR_TYPES[kind], stack
                                )
                            except (ValueError, OSError, Blocked) as e:
                                raise Blocked(
                                    "unavailable directory or wrong right", name
                                ) from e
                            sensitive = True
                        else:
                            path = Path(value)
                            handle = self.pinned(
                                self.pinned_directories, (case["id"], name), value
                            )
                            stat = os.fstat(handle)
                            sensitive = True
                        dirs.append((DIR_TYPES[kind], stat.st_dev, stat.st_ino, path))
                    else:
                        raise Blocked("wrong provider or right", name)
                except (ValueError, OSError) as error:
                    raise Blocked("invalid or unavailable provider", name) from error
            invocation = Invocation(case, words, fixture, dirs, sensitive, stack)
            invocation.bind_ns = time.monotonic_ns() - start
            return invocation
        except BaseException:
            stack.close()
            raise


def request(entries, catalog):
    if not isinstance(entries, list) or not 0 < len(entries) <= MAX_BATCH:
        raise ValueError("run needs a bounded nonempty batch")
    checked = []
    ids = set()
    for entry in entries:
        if not isinstance(entry, dict) or entry.keys() - {
            "id",
            "case",
            "args",
            "input_hex",
        }:
            raise ValueError("unknown request field")
        rid = entry.get("id")
        if (
            not isinstance(rid, str)
            or not re.fullmatch("[A-Za-z0-9_.-]{1,64}", rid)
            or rid in ids
        ):
            raise ValueError("invalid or duplicate request ID")
        ids.add(rid)
        case = (
            catalog.get(entry.get("case"))
            if isinstance(entry.get("case"), str)
            else None
        )
        if case is None:
            raise ValueError("unknown case")
        inputs = {}
        if "args" in entry:
            names = [s["name"] for s in case["slots"] if s["type"] == "U64"]
            args = entry["args"]
            if not isinstance(args, list) or len(args) != len(names):
                raise ValueError("wrong scalar arity")
            for name, word in zip(names, args):
                unsigned(word)
                inputs[name] = "word", word
        if "input_hex" in entry:
            names = [s["name"] for s in case["slots"] if s["type"] == "&Console<Read>"]
            if len(names) != 1:
                raise ValueError("case has no byte input slot")
            hexadecimal(entry["input_hex"])
            inputs[names[0]] = "bytes", entry["input_hex"]
        checked.append((rid, case, inputs))
    return checked


class Selection:
    def __init__(self, checked, grants):
        self.checked = checked
        self.grants = grants
        self.defaults = [{} for _ in checked]
        self.attempted = set()
        self.bound = {}
        self.queued = []
        self.outcomes = {}
        self.resources = contextlib.ExitStack()

    def commands(self, data, binding=False):
        stream = io.BytesIO(data)

        def read(n):
            value = stream.read(n)
            if len(value) != n:
                raise ValueError("incomplete test scheduling request")
            return value

        def word():
            return int.from_bytes(read(8), "little")

        def string():
            size = word()
            if size > 8192:
                raise ValueError("test scheduling argument exceeds bound")
            return read(size).decode("utf-8")

        while tag := stream.read(1):
            op = tag[0]
            if op == 11:
                root = string()
                if not binding or stream.read(1):
                    raise ValueError("invalid named root query")
                return int(root in self.grants.roots)
            ticket = word()
            if ticket >= len(self.checked):
                raise ValueError("descriptor outside selection")
            rid, case, inputs = self.checked[ticket]
            if op == 5:
                if ticket not in self.bound or ticket in self.queued:
                    raise ValueError("only an unused bound invocation can run")
                self.queued.append(ticket)
                continue
            if ticket in self.attempted:
                raise ValueError("descriptor already bound")
            if op in (1, 2, 3, 7, 8, 9, 10):
                name = string()
                if op in (1, 7):
                    provider = ("word", str(word()))
                elif op in (2, 8):
                    provider = ("bytes", string())
                elif op in (3, 9):
                    provider = ("fixture", string() + ":" + string())
                else:
                    provider = ("subdir", (string(), string()))
                if op >= 7 and not any(
                    slot["name"] == name and provider[0] in PROVIDERS[slot["type"]]
                    for slot in case["slots"]
                ):
                    continue
                if name in self.defaults[ticket]:
                    raise ValueError("ambiguous provider for " + name)
                self.defaults[ticket][name] = provider
                continue
            if op != 4 or not binding or stream.read(1):
                raise ValueError("invalid bind request")
            self.attempted.add(ticket)
            try:
                invocation = self.grants.bind(case, inputs, self.defaults[ticket])
            except Blocked as error:
                self.outcomes[ticket] = error.outcome()
                return next(
                    (
                        i + 1
                        for i, slot in enumerate(case["slots"])
                        if slot["name"] == error.slot
                    ),
                    255,
                )
            self.resources.callback(invocation.resources.close)
            self.bound[ticket] = invocation
            return 0
        if binding:
            raise ValueError("missing bind request")


def scheduled(checked, grants, host, sites):
    selection = Selection(checked, grants)
    with selection.resources:
        data = struct.pack("<H", len(checked)) + bytes(
            c["number"] for _, c, _ in checked
        )
        status, ns, commands = host.run(
            {"group": 0, "number": 255},
            [],
            data,
            [],
            lambda data: selection.commands(data, True),
        )
        if status:
            raise ValueError("package test entry failed before dispatch")
        selection.commands(commands)
        for i in selection.queued:
            start = time.monotonic_ns()
            row = selection.bound[i].run(host, sites)
            selection.outcomes[i] = {
                **row,
                "wall_ns": time.monotonic_ns() - start + selection.bound[i].bind_ns,
            }
        for i, (rid, case, _) in enumerate(checked):
            outcome = selection.outcomes.get(i, {"status": "unrun", "entered": False})
            yield {"id": rid, "case": case["id"], **outcome}


def dispatch(checked, grants, host, sites):
    if host.schedule:
        yield from scheduled(checked, grants, host, sites)
        return
    for rid, case, inputs in checked:
        start = time.monotonic_ns()
        try:
            invocation = grants.bind(case, inputs)
        except Blocked as error:
            row = error.outcome()
        else:
            row = invocation.run(host, sites)
        yield {
            "id": rid,
            "case": case["id"],
            **row,
            "wall_ns": time.monotonic_ns() - start,
        }


def batch(entries, catalog, grants, host, sites):
    yield from checked_outcomes(request(entries, catalog), grants, host, sites)


def checked_outcomes(checked, grants, host, sites):
    pending = {rid: case["id"] for rid, case, _ in checked}
    try:
        for row in dispatch(checked, grants, host, sites):
            # The case's own time: what the host measured from entering it, without the
            # host's start-up or the module's compilation, which the first case of a run
            # would otherwise pay inside its deadline. A case never entered has only its
            # binding's wall time.
            if row.get("host_ns", row.get("wall_ns", 0)) >= host.case_deadline_ms * 1_000_000:
                row.update(status="limit", reason=f"{host.case_deadline_ms} ms invocation deadline")
            del pending[row["id"]]
            yield row
    except (OSError, ValueError) as error:
        host.abort()
        for rid, case in pending.items():
            yield {
                "id": rid,
                "case": case,
                "status": "interrupted",
                "entered": None,
                "reason": str(error),
            }


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def service(handshake, catalog, grants, host, sites):
    send(handshake)
    while line := sys.stdin.buffer.readline(2_000_001):
        if len(line) > 2_000_000:
            raise ValueError("session request exceeds byte limit")
        try:
            msg = json.loads(line)
            if msg == {"op": "list"}:
                send({"v": 1, "cases": list(catalog.values())})
                continue
            if (
                not isinstance(msg, dict)
                or msg.keys() != {"op", "entries"}
                or msg["op"] != "run"
            ):
                raise ValueError("unknown session operation or fields")
            checked = request(msg["entries"], catalog)
        except (ValueError, TypeError) as error:
            send({"v": 1, "error": str(error)})
            continue
        counts = {}
        for row in checked_outcomes(checked, grants, host, sites):
            send({"v": 1, **row})
            counts[row["status"]] = counts.get(row["status"], 0) + 1
        send({"v": 1, "done": counts, "compiles": host.compiles, "stores": host.stores})


class SuiteDeadline(Exception):
    pass


def run(args, api):
    # Leave headroom for interpreter startup and reaping inside the five-second default.
    budget = 60 if args.session else 4.85
    pending = []

    def expire(*_):
        raise SuiteDeadline(
            "suite deadline; unfinished: "
            + (", ".join(pending) if pending else "package admission")
        )

    previous = signal.signal(signal.SIGALRM, expire)
    signal.setitimer(
        signal.ITIMER_REAL, max(0.001, api._COMMAND_START + budget - time.monotonic())
    )
    try:
        return run_suite(args, api, pending)
    except SuiteDeadline as error:
        if args.json:
            send(
                {
                    "status": "limit",
                    "unfinished": pending,
                    "elapsed_ns": int((time.monotonic() - api._COMMAND_START) * 1e9),
                }
            )
        api.say(f"test: {error}")
        return 2
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


# A compiler diagnostic's code, as either compiler writes it: `PATH:LINE:COL: W0401: message`,
# or the earlier `PATH:LINE:COL: W0401 12..19: message` with the byte range.
DIAGNOSTIC_CODE = r"W[0-9]{4}(?: [0-9]+\.\.[0-9]+)?: "


def primary_diagnostics(lines):
    return [line for line in lines if re.search(r"(?:^|: )" + DIAGNOSTIC_CODE, line)]


def diagnostic_at(line, path, row, column, code):
    """Whether `line` is the diagnostic `code` at PATH:ROW:COLUMN, in either format."""
    return re.match(re.escape(f"{path}:{row}:{column}: {code}") + r"(?: [0-9]+\.\.[0-9]+)?: ", line) is not None


def compile_process(command, env, root, *, diagnostics=False):
    process = subprocess.Popen(
        command,
        env=env,
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE if diagnostics else None,
        start_new_session=True,
    )
    try:
        out, err = process.communicate()
    except BaseException:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        raise
    return process.returncode, out, err


def rejection(args, manifest, entry, root, compiler, env, current):
    path = args.reject
    if (
        path.parts[:2] != ("tests", "reject")
        or ".." in path.parts
        or path.suffix != ".wy"
    ):
        raise ValueError("reject selects a source below tests/reject")
    expected = re.fullmatch(
        r"(W[0-9]{4})@([1-9][0-9]*):([1-9][0-9]*)", args.expect or ""
    )
    if not expected or (args.lemma and expected[1] != "W0402"):
        raise ValueError(
            "supply an exact --expect CODE@LINE:COLUMN and --lemma for a proof refusal"
        )
    src = manifest.parent / path

    def read():
        fd = test_sources.opened_directory(src.parent)
        try:
            return test_sources.regular(fd, src.name, 1 << 20)
        finally:
            os.close(fd)

    data = read()
    consumer = entry.parent / path
    consumer.parent.mkdir(parents=True, exist_ok=True)
    consumer.write_bytes(data)
    for suffix in (".packages", ".policy"):
        side = entry.with_suffix(suffix)
        if side.exists():
            consumer.with_suffix(suffix).write_bytes(side.read_bytes())
    output = root / "consumer.wasm"
    status, _, err = compile_process(
        [compiler, "package-test-consumer", consumer, entry.parent, output],
        env,
        root,
        diagnostics=True,
    )
    current()
    if read() != data:
        raise ValueError("rejection input changed during checking")
    lines = err.decode(errors="replace").splitlines()
    diagnostics = primary_diagnostics(lines)
    proofs = [line for line in lines if line.startswith("proof failed:")]
    hit = (
        status == 1
        and not output.exists()
        and len(diagnostics) == 1
        and diagnostic_at(diagnostics[0], consumer, expected[2], expected[3], expected[1])
        and proofs == ([f"proof failed: lemma `{args.lemma}`"] if args.lemma else [])
    )
    send(
        {
            "status": "expected_refusal" if hit else "mismatch",
            "entry": path.as_posix(),
            "expected": args.expect,
            "lemma": args.lemma,
            "guest_invocations": 0,
            "published": output.exists(),
            "diagnostics": [
                d.replace(str(entry.parent) + "/", "") for d in diagnostics
            ],
        }
    )
    if not hit and not diagnostics and status:
        raise ValueError(
            "consumer compiler failed without the expected primary diagnostic"
        )
    return 0 if hit else 1


def run_suite(args, api, pending):
    manifest = (
        args.project / "witchy.toml" if args.project.is_dir() else args.project
    ).resolve()
    entry = manifest.parent / "test.wy"
    manifest_bytes = api.read_control_file(manifest)
    doc = tomllib.loads(manifest_bytes.decode("utf-8"))
    lock = manifest.with_name("witchy.lock")
    lock_bytes = api.read_lock_bytes(lock)
    api.check_toolchain(manifest, lock_bytes=lock_bytes)
    if args.dependency:
        registry, deps = api.read_dependencies(manifest, project=doc)
        with tempfile.TemporaryDirectory(
            prefix="grimoire-dependency-tests-"
        ) as directory:
            graph = api.read_lock(manifest, deps, registry, directory, lock_bytes=lock_bytes) if deps else {}
            if args.dependency not in graph:
                raise ValueError("unknown locked dependency")
            raise ValueError(
                "tests-unavailable: this signed release format contains one production source"
            )
    if not entry.is_file() or entry.is_symlink():
        raise ValueError(
            "package needs a regular test.wy entry (use --projects for [[test]])"
        )
    env = api.toolchain_environment()
    compiler = api.tool("WITCHYC", "witchyc")
    executable = api.tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")
    with tempfile.TemporaryDirectory(prefix="grimoire-tests-") as directory:
        root = Path(directory).resolve()
        if not env.get("WITCHY_PROOF_CACHE"):
            proof_cache = root / "accepted-proofs"
            proof_cache.mkdir(mode=0o700)
            env["WITCHY_PROOF_CACHE"] = str(proof_cache)
        entry, current, origin = test_sources.stage(api, manifest, root, doc, lock_bytes)
        if args.reject:
            return rejection(args, manifest, entry, root, compiler, env, current)
        if args.expect or args.lemma:
            raise ValueError("--expect and --lemma select a rejection")
        production = []
        is_app = "run" in doc
        release = (
            doc.get("run", {}).get("entry")
            if is_app
            else doc.get("package", {}).get("source")
        )
        if release is not None:
            target = entry.parent / release
            if (
                Path(release).is_absolute()
                or ".." in Path(release).parts
                or not target.is_file()
                or target.samefile(entry)
                or target.is_relative_to(entry.parent / "tests")
            ):
                raise ValueError("invalid production entry")
            if is_app:
                ordinary = target
            else:
                if any(c in release for c in ('"', "\n", "\r")):
                    raise ValueError("invalid package source name")
                ordinary = entry.parent / "__library_check.wy"
                with ordinary.open("x") as f:
                    f.write(f'import "{release}"\nfn main():\n    return\n')
                for suffix in (".policy", ".packages", ".direct"):
                    source = target.with_suffix(suffix)
                    if source.exists():
                        ordinary.with_suffix(suffix).write_bytes(source.read_bytes())
            production = ["--production", ordinary]
        image_root = root / "images"
        command = [
            compiler,
            ("package-tests-coverage" if args.coverage else "package-tests"),
            entry,
            image_root,
            *production,
        ]
        started = time.monotonic_ns()
        status, raw, said = compile_process(command, env, root, diagnostics=True)
        # In the author's paths: the suite is checked from a staged copy, and a diagnostic
        # that names the copy names a file that is gone when it is read.
        authored = os.path.relpath(manifest.parent)
        authored = "" if authored == "." else authored + "/"
        staged = [(str(entry.parent) + "/", authored), (str(root / "dependencies") + "/", "@")]
        said = said.decode("utf-8", "replace")
        for copy, authored in staged:
            said = said.replace(copy, authored)
        api.relay(said)
        if status:
            raise ValueError("compiler refused package admission")
        compiler_ns = time.monotonic_ns() - started
        current()
        if manifest_bytes != api.read_control_file(manifest) or lock_bytes != api.read_lock_bytes(lock):
            raise ValueError("package manifest or frozen lock changed during checking")
        index = json.loads(raw)
        catalog = {c["id"]: c for c in sorted(index["cases"], key=lambda c: c["id"])}
        if args.property:
            args.case = args.property
        if args.replay:
            args.replay_token = property_trials.read_token(args.replay)
            args.case = args.replay_token["case"]
        if args.save_replay and not args.property:
            raise ValueError("--save-replay requires a property campaign")
        if args.case and args.case not in catalog:
            raise ValueError("unknown selected case")
        all_cases = catalog
        catalog = {
            k: v
            for k, v in catalog.items()
            if (not args.case or k == args.case)
            and (not args.filter or fnmatch.fnmatchcase(k, args.filter))
        }
        modules = index["modules"]
        release_checks = [index["production"]] if index["production"] else []
        owner = {
            "path": str(manifest.parent),
            "device": manifest.parent.stat().st_dev,
            "inode": manifest.parent.stat().st_ino,
        }
        checker = env.get("WITCHY_WASM_READY_BINDING")
        tool_ids = [
            digest(Path(exe).read_bytes())
            for exe in (compiler, executable, api.__file__)
        ] + [api.TEST_SUPERVISOR, *api.TEST_HELPERS.values()]
        artifact = digest(
            json.dumps(
                [
                    owner,
                    origin,
                    digest(manifest_bytes),
                    digest(lock_bytes or b""),
                    tool_ids,
                    api.host_libraries(executable),
                    [m["receipt"]["source_sha256"] for m in release_checks],
                    digest(Path(checker).read_bytes()) if checker else None,
                    index["graph"],
                    index["cases"],
                    index["sites"],
                    [m["sha256"] for m in modules],
                    [m["receipt"].get("source_sha256") for m in modules],
                ],
                separators=(",", ":"),
            ).encode()
        )
        if args.artifact and args.artifact != artifact:
            raise ValueError("selected artifact differs from requested pin")
        handshake = {
            "v": 1,
            "artifact": artifact,
            "owner": (
                {"identity": origin}
                if args.read_dir or args.write_dir or args.read_root or args.write_root
                else owner
            ),
            "features": ["list", "run"],
            "limits": {"batch": MAX_BATCH, "input_bytes": MAX_INPUT},
        }
        for i, module in enumerate(modules):
            checked = ",".join(
                json.dumps(c, separators=(",", ":"), ensure_ascii=False)
                for c in index["cases"]
                if c["group"] == i
            )
            if digest(checked.encode()) != module["index"]:
                raise ValueError("case catalog differs from the compiled index")
        case_deadline = args.case_deadline_ms
        if case_deadline is None:
            table = doc.get("tests", {})
            if not isinstance(table, dict) or set(table) - {"case-deadline-ms"}:
                raise ValueError("[tests] takes case-deadline-ms")
            case_deadline = table.get("case-deadline-ms", CASE_DEADLINE_MS)
        if type(case_deadline) is not int or not 25 <= case_deadline <= 4000:
            raise ValueError("the case deadline is a number of milliseconds from 25 to 4000")
        host = Host(executable, {}, image_root, modules, case_deadline)
        host.coverage = args.coverage
        if args.coverage:
            if args.read_dir or args.write_dir or args.read_root or args.write_root:
                raise ValueError("coverage requires public hermetic inputs")
            handshake["features"].append("guest-edges/1")
            handshake["edge_maps"] = [
                m["receipt"].get("guest_edges_v1", []) for m in modules
            ]
        host.schedule = index["schedule"]
        current()
        if args.proof:
            found = {}
            for module in modules + release_checks:
                for obligation in module["receipt"]["obligations"]:
                    if obligation["declaration"] == args.proof:
                        found[obligation["statement_sha256"]] = {
                            k: obligation[k]
                            for k in ("declaration", "statement_sha256", "outcome")
                        }
            if not found:
                raise ValueError("no checked obligation for " + args.proof)
            send({"proofs": list(found.values()), "guest_invocations": 0})
            return 0
        if args.list:
            if args.json:
                send({**handshake, "cases": list(catalog.values())})
            else:
                for case in catalog.values():
                    print(
                        case["id"],
                        ", ".join(s["name"] + ": " + s["type"] for s in case["slots"]),
                    )
            return 0
        resources = contextlib.ExitStack()

        def directory(path, *, retain):
            try:
                return api.check_directory(path, retain=retain)
            except api.ProjectError as error:
                raise ValueError("directory authorization refused") from error

        grants = TestGrants(args, all_cases, directory, resources)
        try:
            if args.property or args.replay:

                def execute(entry):
                    return next(
                        iter(batch([entry], catalog, grants, host, index["sites"]))
                    )

                report, status = property_trials.run(
                    args, artifact, catalog[args.case], execute
                )
                send(report)
                return status
            if args.session:
                service(handshake, catalog, grants, host, index["sites"])
                return 0
            entries = [{"id": str(i), "case": k} for i, k in enumerate(catalog)]
            rows = []
            pending.extend(catalog)
            for offset in range(0, len(entries), MAX_BATCH):
                for row in batch(
                    entries[offset : offset + MAX_BATCH],
                    catalog,
                    grants,
                    host,
                    index["sites"],
                ):
                    pending.remove(row["case"])
                    rows.append(row)
        finally:
            resources.close()
            host.close()
        work = {
            "compiler_ns": compiler_ns,
            "elapsed_ns": int((time.monotonic() - api._COMMAND_START) * 1e9),
            "compiles": host.compiles,
            "stores": host.stores,
            "proof_obligations": [len(m["receipt"]["obligations"]) for m in modules],
            "production_obligations": [
                len(m["receipt"]["obligations"]) for m in release_checks
            ],
            "proof_requests": sum(
                m["work"]["lean_invocations"] for m in modules + release_checks
            ),
        }
        if args.json:
            send({**handshake, "outcomes": rows, "work": work})
        else:
            for row in rows:
                print(
                    row["status"],
                    row["case"],
                    row.get("reason", ""),
                    row.get("site", ""),
                    *([row["detail"]] if row.get("detail") else []),
                )
        if not rows or any(
            r["status"] in {"blocked", "limit", "unrun", "interrupted"} for r in rows
        ):
            return 2
        return int(any(r["status"] != "pass" for r in rows))
