"""File, listener and process boundaries of the local HTTP example."""

import contextlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import unittest

from registry_run import capture_children, load_cli

ROOT = Path(__file__).resolve().parents[1]
EXAMPLE = ROOT / "examples/http-file-server"
GRIMOIRE = ROOT / "grimoire"


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def get(port, target, timeout=5, hosts=None):
    """The whole reply to `GET target HTTP/1.0`, sent as written (no normalizing of `..`), with
    a `Host` header for each of `hosts`: by default the one naming the granted listener."""
    hosts = [f"127.0.0.1:{port}"] if hosts is None else hosts
    with socket.create_connection(("127.0.0.1", port), timeout=timeout) as connection:
        connection.sendall((f"GET {target} HTTP/1.0\r\n" + "".join(f"Host: {host}\r\n" for host in hosts) + "\r\n").encode())
        reply = b""
        while chunk := connection.recv(4096):
            reply += chunk
        return reply


class HttpFileServer(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.root = Path(cls.temp.name).resolve()
        cls.env = dict(os.environ, XDG_STATE_HOME=str(cls.root / "state"))
        for part in ("httpd", "app"):
            shutil.copytree(EXAMPLE / part, cls.root / part, symlinks=True)
        cls.port = free_port()
        cls.app = cls.root / "app"
        (cls.app / "main.packages").write_text("httpd 1.0.0: ../httpd/httpd.wy\n")
        grants = cls.app / "main.grants.toml"
        grants.write_text(grants.read_text().replace("127.0.0.1:8417", f"127.0.0.1:{cls.port}"))

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    @classmethod
    def inline(cls, *args, env=None, descriptors=()):
        from unittest.mock import patch
        env = cls.env if env is None else env
        module = load_cli()
        # The descriptors a launch inherited (`fd:N` sources): none unless the case gives them.
        module.INHERITED_DESCRIPTORS = frozenset(descriptors)
        # What `main` takes out of the launch environment (without overwriting this process's).
        launch = {name: value.encode("latin-1") if isinstance(value, str) else value
                  for name, value in env.items() if name not in module.TOOLCHAIN_ENVIRONMENT}
        with contextlib.chdir(cls.root), patch.dict(os.environ, env, clear=True), capture_children() as (out, err):
            status = module.cli_main(list(map(str, args)), launch)
            out.seek(0)
            err.seek(0)
            return subprocess.CompletedProcess(args, status, out.read(), err.read())

    def project(self, name):
        """A sibling app with the same package paths."""
        copy = self.root / name
        shutil.copytree(self.app, copy, symlinks=True)
        return copy

    def refused(self, project, message):
        result = self.inline("run", project.name)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stderr)
        return result

    def test_a_client_gets_the_file_and_the_package_reaches_nothing_beside_it(self):
        public = self.app / "site/public"
        # Links in the served tree, to the file beside the granted directory and to one inside it:
        # the engine follows no link, so neither is served.
        (public / "leak.txt").symlink_to("../../secret.txt")
        (public / "alias.txt").symlink_to("hello.txt")
        self.addCleanup((public / "leak.txt").unlink)
        self.addCleanup((public / "alias.txt").unlink)
        record = self.root / "build.json"
        server = subprocess.Popen([GRIMOIRE, "run", "--record", record, "app"], cwd=self.root, env=self.env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
        try:
            deadline = time.monotonic() + 120
            while True:
                try:
                    reply = get(self.port, "/hello.txt")
                    break
                except OSError:
                    if server.poll() is not None or time.monotonic() > deadline:
                        self.fail(f"the server did not start: {server.communicate()[1]}")
                    time.sleep(0.02)
            self.assertEqual(reply, b"HTTP/1.0 200 OK\r\n\r\nhello from witchy\n")
            # What the program has written is here while it still serves, not when it ends.
            import select
            self.assertTrue(select.select([server.stdout], [], [], 10)[0], "the server's first line is not on its standard output while it runs")
            self.assertEqual(server.stdout.readline(), "serving\n")
            not_found = b"HTTP/1.0 404 Not Found\r\n\r\n"
            for target in ["/../secret.txt", "/../../secret.txt", "/%2e%2e/secret.txt", "/leak.txt",
                           "/alias.txt", "/../private.txt", "/missing.txt"]:
                with self.subTest(target=target):
                    self.assertEqual(get(self.port, target), not_found)
            # Only under the granted listener's own name: what a browser sends after a page's
            # name was rebound to this address is another name, and is served nothing.
            misdirected = b"HTTP/1.0 421 Misdirected Request\r\n\r\n"
            bad = b"HTTP/1.0 400 Bad Request\r\n\r\n"
            for hosts, refusal in [(["rebound.example"], misdirected), ([f"rebound.example:{self.port}"], misdirected),
                                   ([f"localhost:{self.port}"], misdirected), (["127.0.0.1"], misdirected),
                                   ([], bad), ([f"127.0.0.1:{self.port}"] * 2, bad),
                                   ([f"127.0.0.1:{self.port}", "rebound.example"], bad)]:
                with self.subTest(hosts=hosts):
                    self.assertEqual(get(self.port, "/hello.txt", hosts=hosts), refusal)
            # No other process takes the address while the engine holds it, whatever sharing it
            # asks for: the connections cannot be split with, or taken by, a second listener.
            for options in [(), (socket.SO_REUSEADDR,), (socket.SO_REUSEPORT,), (socket.SO_REUSEADDR, socket.SO_REUSEPORT)]:
                with self.subTest(options=options), socket.socket() as thief:
                    for option in options:
                        thief.setsockopt(socket.SOL_SOCKET, option, 1)
                    with self.assertRaises(OSError):
                        thief.bind(("127.0.0.1", self.port))
            # A separate client: curl, when it is installed.
            if shutil.which("curl"):
                fetched = subprocess.run(["curl", "-sS", "--max-time", "5", f"http://127.0.0.1:{self.port}/hello.txt"],
                                         capture_output=True, text=True, timeout=10)
                self.assertEqual(fetched.stdout, "hello from witchy\n", fetched.stderr)
        finally:
            os.killpg(server.pid, signal.SIGTERM)
            _, err = server.communicate(timeout=30)
        # The launcher stopped the engine with it: nothing of the process group is left.
        with self.assertRaises(ProcessLookupError):
            os.killpg(server.pid, 0)
        # Exactly what the executable's document grants: one loopback listener, one read-only
        # directory opened without links, the clock; and no outbound network.
        flags = json.loads(record.read_text())["host_flags"]
        self.assertEqual(flags, ["--allocator", "--output", f"--listen=127.0.0.1:{self.port}", "--clock",
                                 f"--dir={self.app / 'site'}", "--no-links", "--dir-id={0.st_dev}:{0.st_ino}".format(os.stat(self.app / "site")), "--data-only", "--stream-output", "--plain-output", "--deadline-ms=300000", "--unmetered",
                                 "--net-bytes=1048576", "--fs-bytes=1048576", "--output-bytes=65536", "--memory-pages=4",
                                 # Each limit under the name its operator set it by (`limit_names`).
                                 *(f"--limit-name={flag}=[run] {key}" for flag, key in (("--deadline-ms", "deadline-ms"), ("--net-bytes", "net-bytes"),
                                                                                     ("--fs-bytes", "file-bytes"), ("--output-bytes", "output-bytes"))),
                                 # What the run used is asked of the engine at every launch.
                                 "--usage"], err)
        # And the run said so before it served anything.
        self.assertIn(f"grant: listener: accepts connections on 127.0.0.1:{self.port} (loopback only), and dials nothing\n", err)
        self.assertIn(f"grant: directory: {self.app / 'site'}, read-only (no symbolic link followed, no other device entered)\n", err)

    def test_the_package_cannot_dial_out(self):
        """A release of httpd that dials out given a Net: no run gets one, so nothing connects."""
        canary = socket.create_server(("127.0.0.1", 0))
        canary.setblocking(False)
        self.addCleanup(canary.close)
        target = f"127.0.0.1:{canary.getsockname()[1]}"
        publisher = self.root / "hostile"
        shutil.copytree(EXAMPLE / "httpd", publisher)
        (publisher / "witchy.toml").write_text((publisher / "witchy.toml").read_text().replace('"1.0.0"', '"1.1.0"'))
        with (publisher / "httpd.wy").open("a") as source:
            source.write(f'\npub fn phone_home(net: &Net) -> U64:\n    match net.connect("{target}"):\n'
                         '        Ok(socket):\n            socket.close()\n            return 1\n'
                         '        Err(error):\n            return 0\n')
        # The package's own grant document asks for the network: nothing reads it. A Coven release
        # is its record and one source file, and grimoire reads only the executable's document.
        (publisher / "net.grants.toml").write_text(f'[net]\nnet = ["{target}"]\n')
        # The release payload also excludes the package's grant document.
        from registry_run import coven_pack
        self.assertEqual(coven_pack(publisher), b"witchy-package 1\nname httpd\nversion 1.1.0\n\n" +
                         (publisher / "httpd.wy").read_bytes())
        app = self.project("dialing")
        (app / "main.packages").write_text("httpd 1.1.0: ../hostile/httpd.wy\n")
        # The executable's author wires a Net to the package, and allows it in the policy.
        main = app / "main.wy"
        main.write_text(main.read_text()
                        .replace("clock: Clock, site: Dir<Read>):", "clock: Clock, site: Dir<Read>, net: Net):\n    let home = phone_home(&net)")
                        )
        policy = app / "main.policy"
        policy.write_text(policy.read_text().replace("@httpd/: Allocator, Clock, Dir<Read>, Socket", "@httpd/: Allocator, Clock, Dir<Read>, Net, Socket"))
        # The document grants no Net: the run stops before anything is built.
        self.refused(app, "net: main takes a Net, and the document grants none ([net].net)")
        # Granted one in the executable's own document: the launcher grants no outbound network.
        with (app / "main.grants.toml").open("a") as document:
            document.write(f'[net]\nnet = ["{target}"]\n')
        self.refused(app, "[net]: grimoire grants no outbound network access")
        with self.assertRaises(BlockingIOError):
            canary.accept()

    def test_the_launcher_grants_one_loopback_listener_and_one_read_only_directory(self):
        """Each refused before the build, naming why."""
        cli = load_cli()
        for address in ("0.0.0.0:8417", "localhost:8417", "192.168.1.1:8417", "127.0.0.2:8417",
                        "127.0.0.1:0", "127.0.0.1:08417",
                        # IPv6: only `::1`, as the compiler writes it in a flag.
                        ":::8417", "::2:8417", "fe80::1:8417", "2001:db8::1:8417", "::ffff:127.0.0.1:8417",
                        "0:0:0:0:0:0:0:1:8417", "[::1]:8417", "::1:0", "::1:08417", "::1", "::1:"):
            with self.subTest(address=address), self.assertRaisesRegex(cli.ProjectError, "a loopback address"):
                cli.check_listener(address)
        cli.check_listener(f"127.0.0.1:{self.port}")
        cli.check_listener(f"::1:{self.port}")
        self.assertEqual(cli.listener_authority(f"::1:{self.port}"), f"[::1]:{self.port}")
        self.assertEqual(cli.listener_authority(f"127.0.0.1:{self.port}"), f"127.0.0.1:{self.port}")
        (self.app / "linked").symlink_to(self.root)
        (self.app / "linked-parent").symlink_to(self.app / "site")
        self.addCleanup((self.app / "linked").unlink)
        self.addCleanup((self.app / "linked-parent").unlink)
        for path, message in (("../elsewhere", "not a plain absolute path"), ("secret.txt", "not a directory"),
                              ("nowhere", "missing"), ("linked", "symbolic link"), ("linked-parent/public", "symbolic link")):
            with self.subTest(path=path), self.assertRaisesRegex(cli.ProjectError, message):
                cli.check_directory(f"{self.app}/{path}")
        # A directory with more than reading. (A second directory or listener is not a program:
        # main takes distinct capabilities, one of each, and the compiler refuses it.)
        for name, kind, rights in [("writable", "Dir<Write>", '["Write"]'), ("read-write", "Dir", '["Read", "Write"]')]:
            with self.subTest(name):
                app = self.project(f"refused-{name}")
                (app / "main.wy").write_text(f"import fs\nfn main(site: {kind}):\n    site.close()\n")
                (app / "main.grants.toml").write_text('[dirs]\nsite = { root = "site" }\n')
                refused = self.refused(app, "main.grants.toml: [dirs] site: main takes a directory it can write, and the entry states no rights")
                # In the document's words, with what to write there: never an engine flag.
                self.assertIn("rights = [", refused.stderr)
                self.assertNotIn("--dir-rights", refused.stderr)
                # Stated, it is a grant the launcher gives, but never of the project's own tree:
                # a program that could write its policy, lock or source could grant itself anything.
                (app / "main.grants.toml").write_text(f'[dirs]\nsite = {{ root = "{app}", rights = {rights} }}\n')
                self.refused(app, "it is the project directory itself")
        app = self.project("refused-two-dirs")
        main = app / "main.wy"
        main.write_text(main.read_text().replace("site: Dir<Read>)", "site: Dir<Read>, more: Dir<Read>)", 1))
        self.refused(app, "main accepts only distinct")
        # Never a descriptor-alias tree, nor a directory holding one: a name there opens a
        # descriptor (a secret's pipe among them).
        for name, root, message in ([("dev", "/dev", "is in the descriptor-alias tree"),
                                    ("dev-fd", "/dev/fd", "is in the descriptor-alias tree"),
                                    ("root", "/", "contains the descriptor-alias tree")]
                                   # macOS: the Data volume has autofs (`home`) mounted below it.
                                   + ([("mount-below", "/System/Volumes/Data", "has a file system mounted below it")]
                                      if os.path.ismount("/System/Volumes/Data/home") else [])):
            with self.subTest(name):
                with self.assertRaisesRegex(cli.ProjectError, message):
                    cli.check_directory(root)
        # Run limits are checked like grants: a bad value is refused, never ignored.
        for name, old, new, message in [("deadline", "deadline-ms = 300000", "deadline-ms = 0", "[run].deadline-ms is a number"),
                                        ("deadline-long", "deadline-ms = 300000", "deadline-ms = 600001", "[run].deadline-ms is a number"),
                                        ("fuel", 'fuel = "unmetered"', "fuel = -1", "[run].fuel is an instruction count"),
                                        ("fuel-text", 'fuel = "unmetered"', 'fuel = "lots"', "[run].fuel is an instruction count"),
                                        ("unknown", 'fuel = "unmetered"', "budget = 5", "invalid [run].budget. Expected one of: entry, deadline-ms, fuel, net-bytes")]:
            with self.subTest(name):
                manifest = self.root / "limits.toml"
                manifest.write_text((self.app / "witchy.toml").read_text().replace(old, new))
                with self.assertRaises(cli.ProjectError) as error:
                    cli.read_limits(manifest)
                self.assertIn(message, str(error.exception))

    def test_a_review_shows_each_grant_of_the_run(self):
        """`grimoire review` lists what a run would be given, a line for each grant and limit,
        from the flags the engine would get; an IPv6 loopback listener is one of them, in
        brackets; and a document a run would refuse is refused by the review."""
        cli = load_cli()
        granted = lambda address, site: [
            "grant: allocator: heap memory inside the module",
            "grant: output: the program writes its standard output",
            f"grant: listener: accepts connections on {address} (loopback only), and dials nothing",
            "grant: clock: the time and timers",
            f"grant: directory: {site}, read-only (no symbolic link followed, no other device entered)",
            "limit: stops after 300000 ms",
            "limit: instructions are not counted",
            "limit: network: 1048576 bytes sent and received in the whole run",
            "limit: files: 1048576 bytes read and written in the whole run",
            "limit: output: 65536 bytes written in the whole run",
            "limit: memory: 4 pages of 64 KiB, the program and its heap together",
            "limit: heap: 98304 bytes, in 3 pages of 64 KiB of program memory (built into the module)",
            "limit: allocations: 64 alive at once (built into the module)",
            "limit: stack: 16384 bytes of call frames (built into the module)"]
        reviewed = self.inline("review", "app")
        self.assertEqual(reviewed.returncode, 0, reviewed.stderr)
        shown = [line for line in reviewed.stdout.splitlines() if line.startswith(("grant: ", "limit: "))]
        self.assertEqual(shown, granted(f"127.0.0.1:{self.port}", self.app / "site"), reviewed.stdout)
        six = self.project("ipv6")
        document = six / "main.grants.toml"
        document.write_text(document.read_text().replace(f"127.0.0.1:{self.port}", f"[::1]:{self.port}"))
        reviewed = self.inline("review", six.name)
        self.assertEqual(reviewed.returncode, 0, reviewed.stderr)
        shown = [line for line in reviewed.stdout.splitlines() if line.startswith(("grant: ", "limit: "))]
        self.assertEqual(shown, granted(f"[::1]:{self.port}", six / "site"), reviewed.stdout)
        # Not loopback, with or without brackets in the way: refused, and nothing is shown as granted.
        for address in (f"[2001:db8::1]:{self.port}", f"[::]:{self.port}", f"[::ffff:127.0.0.1]:{self.port}"):
            with self.subTest(address=address):
                document.write_text(document.read_text().replace(f"[::1]:{self.port}", address))
                refused = self.inline("review", six.name)
                self.assertNotEqual(refused.returncode, 0, refused.stdout)
                self.assertIn("a loopback address", refused.stderr)
                self.assertNotIn("grant: ", refused.stdout)
                # Still reviewable: the policy comparison ran, and the digests can be pinned.
                self.assertIn("a run would be refused", refused.stderr)
                self.assertEqual(self.inline("review", "--accept", six.name).returncode, 0)
                document.write_text(document.read_text().replace(address, f"[::1]:{self.port}"))
        # Secrets are named with their use and source, never a value; a withheld output is not
        # listed; a flag this does not know is shown as it is; nothing granted says so.
        self.assertEqual(cli.describe_grants(["--secret-slot=key:seal:env:DEMO_KEY", "--secret=TOKEN", "--secret-seal=SIGNING",
                                              "--file=/srv/app/config.toml", "--no-links", "--fuel=5000", "--future=1"]),
                         ["grant: secret key: seal, read from env:DEMO_KEY",
                          "grant: store secret TOKEN: read and sign, read from the launch variable TOKEN",
                          "grant: store secret SIGNING: sign only, read from the launch variable SIGNING",
                          "grant: file: /srv/app/config.toml, read-only",
                          "grant: paths: no granted path goes through a symbolic link",
                          "limit: stops after 5000 instructions",
                          "grant: --future=1"])
        self.assertEqual(cli.describe_grants(["--deadline-ms=1000"]),
                         ["grant: nothing: the program reaches nothing outside itself", "limit: stops after 1000 ms"])

    def test_no_engine_outlives_the_launcher(self):
        """Grimoire alone is signalled: SIGHUP and SIGQUIT are caught. After SIGKILL the host
        reports that its lifeline closed, with or without a deadline. The port closes and no
        temporary directory of the run (its staged sources, snapshot and module) is on disk while
        the engine serves, so none outlives a launcher killed by SIGKILL."""
        unbounded = self.project("no-deadline")
        manifest = unbounded / "witchy.toml"
        manifest.write_text(manifest.read_text().replace("deadline-ms = 300000", 'deadline-ms = "none"'))
        for number, app in ((signal.SIGHUP, "app"), (signal.SIGQUIT, "app"), (signal.SIGKILL, "app"), (signal.SIGKILL, unbounded.name)):
            with self.subTest(signal.Signals(number).name, app=app):
                temporary = self.root / f"tmp-{signal.Signals(number).name}-{app}"
                temporary.mkdir(mode=0o700)
                staged = lambda: sorted(entry.name for entry in temporary.iterdir() if entry.name.startswith("grimoire-"))
                diagnostics = temporary / "stderr"
                with diagnostics.open("wb") as stderr:
                    launcher = subprocess.Popen([GRIMOIRE, "run", app], cwd=self.root, env=dict(self.env, TMPDIR=str(temporary)),
                                                stdout=subprocess.DEVNULL, stderr=stderr, start_new_session=True)
                try:
                    deadline = time.monotonic() + 120
                    while True:
                        try:
                            get(self.port, "/hello.txt")
                            break
                        except OSError:
                            if launcher.poll() is not None:
                                self.fail(f"the server did not start\n{diagnostics.read_text()}")
                            self.assertLess(time.monotonic(), deadline, "the server did not start")
                            time.sleep(0.02)
                    self.assertEqual(staged(), [], "the run's temporary directory is on disk while the engine serves")
                    launcher.send_signal(number)
                    status = launcher.wait(timeout=30)
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        try:
                            socket.create_connection(("127.0.0.1", self.port), timeout=1).close()
                        except OSError:
                            break
                        time.sleep(0.02)
                    else:
                        self.fail(f"the engine still listens after {signal.Signals(number).name} to the launcher")
                    if number == signal.SIGKILL:
                        self.assertEqual(status, -signal.SIGKILL)
                        self.assertIn("witchy: the launcher is gone: the run ends with it\n", diagnostics.read_text())
                    self.assertEqual(staged(), [], f"a temporary directory outlived {signal.Signals(number).name} to the launcher")
                finally:
                    try:
                        os.killpg(launcher.pid, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        # macOS refuses (EPERM) a group whose members have all exited and are not
                        # yet reaped: the engine, reparented, waits for launchd. Nothing to kill.
                        pass
                    launcher.wait()

    def test_no_directory_grant_while_the_program_takes_secrets(self):
        """A secret's source file inside the granted directory (or any secret at all, an `env:`
        one with no file to compare) refuses the directory grant before the build."""
        app = self.project("secret-in-tree")
        main = app / "main.wy"
        main.write_text(main.read_text().replace("site: Dir<Read>):", "site: Dir<Read>, key: Secret<Seal>):", 1))
        key = app / "site/public/key.bin"
        key.write_bytes(os.urandom(32))
        # The key's file on a descriptor of this launch, as `fd:N` (inheriting one across the
        # launch itself is SecretRun's process-boundary case).
        opened = os.open(key, os.O_RDONLY)
        self.addCleanup(os.close, opened)
        with (app / "main.grants.toml").open("a") as document:
            document.write(f'[secrets]\nkey = {{ use = "seal", from = "fd:{opened}", also_from = ["env:HTTP_DEMO_KEY"] }}\n')
        result = self.inline("run", app.name, descriptors=(opened,))
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("refused while the program takes secrets", result.stderr)
        result = self.inline("run", app.name, "--bind", "key=env:HTTP_DEMO_KEY", env=dict(self.env, HTTP_DEMO_KEY="k" * 32))
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("refused while the program takes secrets", result.stderr)
        # A directory to write is refused the same way, before its rights are looked at.
        main.write_text(main.read_text().replace("site: Dir<Read>, key: Secret<Seal>):", "site: Dir, key: Secret<Seal>):", 1))
        document = app / "main.grants.toml"
        document.write_text(document.read_text().replace('rights = ["Read"]', 'rights = ["Read", "Write"]'))
        result = self.inline("run", app.name, "--bind", "key=env:HTTP_DEMO_KEY", env=dict(self.env, HTTP_DEMO_KEY="k" * 32))
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("refused while the program takes secrets", result.stderr)


if __name__ == "__main__":
    unittest.main()
