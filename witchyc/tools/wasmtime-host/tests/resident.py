"""The resident runner (`--features resident`, src/resident.rs): what one run is given never
reaches the next, and a run there behaves as the executable's run does.

    WITCHYC=... WITCHY_WASMTIME_HOST=... WITCHY_WASMTIME_RESIDENT=... \
    WITCHY_WASM_READY_BINDING=... WITCHY_PROOF_CACHE=... python3 tools/wasmtime-host/tests/resident.py

WITCHY_WASMTIME_HOST is a default build and WITCHY_WASMTIME_RESIDENT one with the feature, of
the same commit. Without them every test fails: nothing here is skipped.
"""
import fcntl
import os
from pathlib import Path
import select
import shutil
import socket
import struct
import subprocess
import tempfile
import termios
import time
import unittest

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "tests" / "fixtures"
TOOLS = {name: os.environ.get(name) for name in ("WITCHYC", "WITCHY_WASMTIME_HOST", "WITCHY_WASMTIME_RESIDENT")}
SLOTS = ["--output", "--allocator", "--secret-slot=alpha:seal:env:FIXTURE_KEY_ALPHA",
         "--secret-slot=token:reveal-bytes:fd:{fd}", "--secret-slot=beta:seal:env:FIXTURE_KEY_BETA"]
KEYS = {"FIXTURE_KEY_ALPHA": "Jefe", "FIXTURE_KEY_BETA": "key"}
CWD = 0xFFFFFFFF
# Prints draws from the host's generator (`Rand`), one per line.
DRAWS = """import console
import rand

fn main(console: Console<Write>, rand: Rand):
    for step in 0..4:
        match rand.below(1000000000000):
            Ok(n):
                console.print_int(n)
            Err:
                return
"""


def request(path, args, env, fds, cwd="/", mask=0o022):
    """A run: [(descriptor, its number in the run)]; returns the connection."""
    directory = os.open(cwd, os.O_RDONLY | os.O_DIRECTORY)
    try:
        given = fds + [(directory, CWD)]
        strings = lambda items: struct.pack("<I", len(items)) + b"".join(struct.pack("<I", len(i)) + i for i in items)
        body = strings([os.fsencode(str(a)) for a in args]) + strings([f"{k}={v}".encode() for k, v in env.items()])
        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        connection.connect(path)
        socket.send_fds(connection, [b"WRS1" + struct.pack("<I", len(given))], [fd for fd, _ in given])
        connection.sendall(struct.pack(f"<{len(given)}I", *(n for _, n in given)) + struct.pack("<II", mask, len(body)) + body)
        return connection
    finally:
        os.close(directory)


def status(connection):
    data = b""
    while len(data) < 8:
        more = connection.recv(8 - len(data))
        if not more:
            connection.close()
            return None
        data += more
    connection.close()
    assert data[:4] == b"WRS1"
    return struct.unpack("<i", data[4:])[0]


def start_resident(path):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(path)
        listener.listen(64)
        server = subprocess.Popen([TOOLS["WITCHY_WASMTIME_RESIDENT"], f"--resident=fd:{listener.fileno()}"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, env={}, pass_fds=(listener.fileno(),))
    assert server.stdout.readline() == b"ready\n"
    return server


class Resident(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        missing = [name for name, path in TOOLS.items() if not path or not os.access(path, os.X_OK)]
        if missing:
            raise RuntimeError(f"set {', '.join(missing)} (see the module's docstring)")
        cls.work = Path(tempfile.mkdtemp(prefix="witchy-resident-test-"))
        cls.slots = cls.build(FIXTURES / "host" / "secret-slots.wy")
        (cls.work / "draws.wy").write_text(DRAWS)
        cls.draws = cls.build(cls.work / "draws.wy")
        cls.socket_path = str(cls.work / "socket")
        cls.server = start_resident(cls.socket_path)

    @classmethod
    def tearDownClass(cls):
        cls.server.stdin.close()
        cls.server.wait(timeout=30)
        cls.server.stdout.close()
        shutil.rmtree(cls.work)

    @classmethod
    def build(cls, source):
        module = cls.work / (source.stem + ".wasm")
        built = subprocess.run([TOOLS["WITCHYC"], "wasm-checked", source, "-o", module], capture_output=True, text=True)
        assert built.returncode == 0, built.stderr
        return module

    def run_in(self, where, module, flags, env=None, token=b"\x00\x80\xff", give=True, hold=False):
        """One run of `module`, in the executable or the resident runner: (status, stdout, stderr).
        `token` arrives on a pipe at the number `{fd}` names (unless not `give`n); `hold`
        keeps its writer open."""
        reading, writing = os.pipe()
        out_r, out_w = os.pipe()
        err_r, err_w = os.pipe()
        null = os.open(os.devnull, os.O_RDONLY)
        args = [TOOLS["WITCHY_WASMTIME_HOST"], module, *[f.replace("{fd}", str(reading)) for f in flags]]
        env = KEYS if env is None else env
        try:
            if where == "executable":
                process = subprocess.Popen(args, env=env, stdin=null, stdout=out_w, stderr=err_w, pass_fds=(reading,) if give else ())
            else:
                connection = request(self.socket_path, args, env, [(null, 0), (out_w, 1), (err_w, 2)] + ([(reading, reading)] if give else []))
        finally:
            for fd in (reading, out_w, err_w, null):
                os.close(fd)
        try:
            os.write(writing, token)
        except BrokenPipeError:
            pass
        if not hold:
            os.close(writing)
        out = b"".join(iter(lambda: os.read(out_r, 65536), b""))
        err = b"".join(iter(lambda: os.read(err_r, 65536), b""))
        os.close(out_r)
        os.close(err_r)
        code = process.wait() if where == "executable" else status(connection)
        if hold:
            os.close(writing)
        return code, out.decode(), err.decode()

    def resident(self, *args, **options):
        return self.run_in("resident", *args, **options)

    def test_a_run_gets_what_the_executable_gets(self):
        for flags, env in [(SLOTS, KEYS), (SLOTS, {}), (["--output"], KEYS), (SLOTS + ["--fuel=1000"], KEYS), (SLOTS + ["--fuel"], KEYS)]:
            self.assertEqual(self.run_in("executable", self.slots, flags, env), self.resident(self.slots, flags, env), flags)

    def test_a_secret_given_to_one_run_is_not_the_next_ones(self):
        code, out, _ = self.resident(self.slots, SLOTS, token=b"\x01\x02")
        self.assertEqual((code, out.splitlines()[2:5]), (0, ["2", "1", "2"]))
        # The same descriptor number, not given: nothing is at it.
        code, out, err = self.resident(self.slots, SLOTS, give=False)
        self.assertEqual((code, out), (1, ""))
        self.assertRegex(err, r"secret slot token: unreadable descriptor \(fd:\d+\)")
        # The variables, not given: absent, though the last run had them.
        code, out, err = self.resident(self.slots, SLOTS, env={})
        self.assertEqual((code, out), (1, ""))
        self.assertIn("secret slot alpha: missing input (env:FIXTURE_KEY_ALPHA)", err)
        # Another value at the same number: that value only.
        code, out, _ = self.resident(self.slots, SLOTS, token=b"\x07")
        self.assertEqual((code, out.splitlines()[2:4]), (0, ["1", "7"]))

    def test_the_resident_process_keeps_no_descriptor_of_a_run(self):
        lsof = shutil.which("lsof")
        self.assertIsNotNone(lsof, "lsof lists the resident process's descriptors")
        listed = lambda: sorted(line.split()[3] for line in subprocess.run([lsof, "-n", "-P", "-p", str(self.server.pid)], capture_output=True, text=True).stdout.splitlines()[1:] if line.split()[3][0].isdigit())
        before = listed()
        for _ in range(5):
            self.assertEqual(self.resident(self.slots, SLOTS)[0], 0)
        self.assertEqual(listed(), before)

    def test_fuel_deadline_and_limits_are_each_runs_own(self):
        fuel = lambda: [line for line in self.resident(self.slots, SLOTS + ["--fuel"])[2].splitlines() if line.startswith("witchy: fuel ")]
        first = fuel()
        self.assertEqual(len(first), 1)
        # Out of fuel, then a run with the default budget: unaffected, and it counts the same.
        self.assertIn("instruction budget ran out", self.resident(self.slots, SLOTS + ["--fuel=1000"])[2])
        self.assertEqual(fuel(), first)
        # A writer held past a short deadline: that run stops; the next has its own deadline.
        started = time.monotonic()
        code, _, err = self.resident(self.slots, SLOTS + ["--deadline-ms=300"], hold=True)
        self.assertNotEqual(code, 0, err)
        self.assertLess(time.monotonic() - started, 5)
        self.assertEqual(self.resident(self.slots, SLOTS)[0], 0)
        # A refused memory grant, then the default one.
        self.assertIn("invalid grant: --memory-pages=1", self.resident(self.slots, SLOTS + ["--memory-pages=1"])[2])
        self.assertEqual(fuel(), first)

    def test_each_run_draws_its_own_randomness(self):
        draws = [self.resident(self.draws, ["--output", "--rand"]) for _ in range(3)]
        self.assertTrue(all(code == 0 and len(out.split()) == 4 for code, out, _ in draws), draws)
        self.assertEqual(len({out for _, out, _ in draws}), 3)

    def test_a_module_is_its_bytes_not_its_path(self):
        path = self.work / "module.wasm"
        for module, flags, expected in [(self.slots, SLOTS, "not text"), (self.draws, ["--output", "--rand"], None), (self.slots, SLOTS, "not text")]:
            shutil.copyfile(module, path)
            code, out, err = self.resident(path, flags)
            self.assertEqual(code, 0, err)
            self.assertEqual(out.splitlines()[-1] == "not text", expected == "not text")
        # A changed byte (in the name section's last name) is a different module: admitted and
        # compiled again, never taken for the one before it.
        data = bytearray(self.slots.read_bytes())
        data[-1] ^= 0x01
        path.write_bytes(bytes(data))
        self.assertEqual(self.run_in("executable", path, SLOTS)[:2], self.resident(path, SLOTS)[:2])

    def test_a_malformed_request_is_refused_and_the_next_is_served(self):
        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        connection.connect(self.socket_path)
        null = os.open(os.devnull, os.O_RDONLY)
        try:
            # Three descriptors announced, one given.
            socket.send_fds(connection, [b"WRS1" + struct.pack("<I", 3)], [null])
        finally:
            os.close(null)
        self.assertIsNone(status(connection))
        self.assertEqual(self.resident(self.slots, SLOTS)[0], 0)

    def test_too_many_descriptors_are_refused_and_the_next_is_served(self):
        # Review: 66 or more crashed the resident process (a truncated descriptor list).
        null = os.open(os.devnull, os.O_RDONLY)
        try:
            for count in (65, 66, 200, 250):
                connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                connection.connect(self.socket_path)
                socket.send_fds(connection, [b"WRS1" + struct.pack("<I", count)], [null] * count)
                self.assertIsNone(status(connection))
        finally:
            os.close(null)
        self.assertIsNone(self.server.poll())
        self.assertEqual(self.resident(self.slots, SLOTS)[0], 0)

    def test_a_descriptor_sent_after_the_first_byte_is_refused_not_kept(self):
        # Review: one sent with a later segment was installed in the resident process and kept.
        reading, writing = os.pipe()
        null = os.open(os.devnull, os.O_RDONLY)
        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        connection.connect(self.socket_path)
        try:
            socket.send_fds(connection, [b"WRS1" + struct.pack("<I", 4)], [null] * 4)
            socket.send_fds(connection, [struct.pack("<4I", 0, 1, 2, CWD)], [reading])
        finally:
            os.close(null)
            os.close(reading)
        self.assertIsNone(status(connection))
        # Its only reader is gone: nothing in the resident process holds it.
        with self.assertRaises(BrokenPipeError):
            os.write(writing, b"x")
        os.close(writing)

    def test_a_silent_connection_holds_up_no_run(self):
        # Review: one blocked every run for the read timeout.
        idle = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        idle.connect(self.socket_path)
        try:
            started = time.monotonic()
            self.assertEqual(self.resident(self.slots, SLOTS)[0], 0)
            self.assertLess(time.monotonic() - started, 1)
        finally:
            idle.close()

    def check_stopped_run(self, socket_path, stop):
        reading, writing = os.pipe()
        err_r, err_w = os.pipe()
        with (os.fdopen(reading, "rb") as token, os.fdopen(writing, "wb") as writer,
              os.fdopen(err_r, "rb") as diagnostics, os.fdopen(err_w, "wb") as stderr,
              open(os.devnull, "rb") as stdin, open(os.devnull, "wb") as stdout):
            writer.write(b"x")
            writer.flush()
            flags = [flag.replace("{fd}", str(reading)) for flag in SLOTS]
            args = [TOOLS["WITCHY_WASMTIME_HOST"], self.slots, *flags, "--deadline-ms=10000"]
            with request(socket_path, args, KEYS, [(stdin.fileno(), 0), (stdout.fileno(), 1),
                         (stderr.fileno(), 2), (token.fileno(), reading)]) as connection:
                stderr.close()
                # The child consumed the first input byte and is blocked waiting for EOF.
                start_deadline = time.monotonic() + 3
                while struct.unpack("i", fcntl.ioctl(token, termios.FIONREAD, bytes(4)))[0]:
                    self.assertLess(time.monotonic(), start_deadline, "the resident run never read its input")
                    time.sleep(0.005)

                stop(connection)
                stopped_deadline = time.monotonic() + 3
                errors = bytearray()
                while True:
                    ready, _, _ = select.select([diagnostics], [], [], max(0, stopped_deadline - time.monotonic()))
                    self.assertTrue(ready, "the stopped run still holds its stderr open")
                    chunk = os.read(diagnostics.fileno(), 4096)
                    if not chunk:
                        break
                    errors.extend(chunk)
                self.assertIn(b"SIGTERM", errors)

    def test_disconnected_caller_stops_its_run(self):
        self.check_stopped_run(self.socket_path, lambda connection: connection.close())
        self.assertEqual(self.resident(self.slots, SLOTS)[0], 0)

    def test_shutdown_reaps_an_active_run(self):
        path = str(self.work / "shutdown")
        with start_resident(path) as server:
            def shutdown(_connection):
                server.stdin.close()
                self.assertEqual(server.wait(timeout=3), 0)
            self.check_stopped_run(path, shutdown)


if __name__ == "__main__":
    unittest.main()
