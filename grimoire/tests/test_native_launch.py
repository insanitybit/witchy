"""Direct checks of the native target: a grant document with `[launch] version = 1` is a launch
plan the executable embeds. What Grimoire reads of it to launch, what it refuses, and what the
executable is given: the declared words, each secret on a pipe named by its declared argument,
only the granted variables, no other descriptor, standard input only when granted. One case
builds and runs a real executable when the compiler can (macOS ARM64)."""
import contextlib
import hashlib
import json
import os
import time
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch

from registry_run import load_cli


PLAN = """[launch]
version = 1

[flags]
notes = { type = "path", required = false, default = { from = "cwd", path = "notes" } }
token-fd = { type = "fd", secret = true }
notes-id = { type = "object" }

[dirs]
notes = { root = { from = "arg", name = "notes" }, rights = ["Read"], identity = { from = "arg", name = "notes-id" } }

[secrets]
token = { use = "reveal-bytes", from = { from = "arg", name = "token-fd" } }

[env]
lang = ["GRANTED_VAR"]
"""

# A stand-in executable: reports its words, environment, descriptors, input and directory.
PROBE = """#!{python}
import json, os, sys
fds = sorted(int(n) for n in os.listdir("/dev/fd") if n.isdigit() and int(n) > 2)
open_fds = []
for n in fds:
    try:
        os.fstat(n)
        open_fds.append(n)
    except OSError:
        pass
secret = None
if "--token-fd" in sys.argv:
    secret = os.read(int(sys.argv[sys.argv.index("--token-fd") + 1]), 100).decode()
stdin = os.fstat(0)
print(json.dumps({{"argv": sys.argv[1:], "env": dict(os.environ), "fds": open_fds, "secret": secret,
                  "stdin_null": os.path.samestat(stdin, os.stat("/dev/null")), "cwd": os.getcwd()}}))
print("witchy: spoofed")
"""


class NativeLaunch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cli = load_cli()

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.manifest = self.root / "witchy.toml"
        self.manifest.write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "main.grants.toml"\n[native]\nsecrets = { token-fd = "env:TOKEN" }\n')
        self.document = self.root / "main.grants.toml"
        self.document.write_text(PLAN)

    def grant(self, text=PLAN):
        import tomllib
        return tomllib.loads(text)

    def test_declared_flags_and_secret_sources(self):
        declared = self.cli.native_flags(self.document, self.grant())
        self.assertEqual(declared, {"notes": ("path", False), "token-fd": ("fd", True), "notes-id": ("object", False)})
        self.assertEqual(self.cli.native_secret_sources(self.manifest, self.document, self.grant(), declared), {"token-fd": "env:TOKEN"})

    def test_refusals(self):
        refused = lambda call: self.assertRaises(self.cli.ProjectError, call)
        # Only version 1.
        refused(lambda: self.cli.native_flags(self.document, self.grant(PLAN.replace("version = 1", "version = 2"))))
        declared = self.cli.native_flags(self.document, self.grant())
        # A secret descriptor with no source, a source for no declared secret, a toolchain
        # variable, one source for two descriptors, a plain descriptor Grimoire would pass on.
        for native in ("", 'secrets = { other = "env:TOKEN", token-fd = "env:TOKEN" }', 'secrets = { token-fd = "env:PATH" }',
                       'secrets = { token-fd = "env:SSL_CERT_FILE" }', 'secrets = { token-fd = "env:WITCHY_X" }',
                       'secrets = { token-fd = "/etc/key" }'):
            self.manifest.write_text(f'[run]\nentry = "main.wy"\n[native]\n{native}\n')
            refused(lambda: self.cli.native_secret_sources(self.manifest, self.document, self.grant(), declared))
        self.manifest.write_text('[run]\nentry = "main.wy"\n[native]\nsecrets = { token-fd = "env:TOKEN" }\n')
        plain = PLAN.replace('token-fd = { type = "fd", secret = true }', 'token-fd = { type = "fd", secret = true }\nraw = { type = "fd" }')
        refused(lambda: self.cli.native_secret_sources(self.manifest, self.document, self.grant(plain), self.cli.native_flags(self.document, self.grant(plain))))
        # A secret the executable would read from its own environment.
        env_secret = PLAN.replace('from = { from = "arg", name = "token-fd" }', 'from = "env:TOKEN"')
        refused(lambda: self.cli.native_secret_sources(self.manifest, self.document, self.grant(env_secret), declared))

    def test_words_are_declared_path_arguments_only(self):
        declared = self.cli.native_flags(self.document, self.grant())
        self.assertEqual(self.cli.native_words(["--notes", "x"], declared), ["--notes", "x"])
        self.assertEqual(self.cli.native_words(["--notes=x"], declared), ["--notes", "x"])
        for words in (["--token-fd", "0"], ["--notes", "x", "--notes", "y"], ["--other", "x"], ["notes"], ["--notes"], ["--output"]):
            self.assertRaises(self.cli.ProjectError, self.cli.native_words, words, declared)

    def test_net_and_exec_are_refused(self):
        for section in ("[net]\napi = [\"127.0.0.1:1\"]\n", "[exec]\nexec = { dir = \"/usr/bin\", programs = [\"true\"] }\n"):
            with self.assertRaises(self.cli.ProjectError):
                self.cli.run_native(self.manifest, self.root / "main.wy", self.document, self.grant(PLAN + section),
                                    {}, False, None, None, [], False, [], document_text=PLAN + section)

    def test_launch_gives_only_what_is_declared(self):
        probe = self.root / "probe"
        probe.write_text(PROBE.format(python=sys.executable))
        probe.chmod(0o755)
        reading, writing = os.pipe()   # a descriptor this process holds: never passed on
        self.addCleanup(os.close, reading)
        self.addCleanup(os.close, writing)
        out = self.root / "out"
        with open(out, "wb") as stream:
            saved = os.dup(1)
            os.dup2(stream.fileno(), 1)
            try:
                self.cli.launch_native(probe, ["--notes", "here"], {"token-fd": b"s3cret"}, False, self.root,
                                       {b"GRANTED_VAR": b"v"}, raw_output=False)
            finally:
                os.dup2(saved, 1)
                os.close(saved)
        report, marked = out.read_text().splitlines()
        seen = json.loads(report)
        self.assertEqual(seen["argv"][:2], ["--notes", "here"])
        self.assertEqual(seen["argv"][2], "--token-fd")
        self.assertEqual(seen["secret"], "s3cret")
        # (The probe's own interpreter adds LC_CTYPE and macOS __CF_USER_TEXT_ENCODING itself.)
        self.assertEqual({k: v for k, v in seen["env"].items() if k not in ("__CF_USER_TEXT_ENCODING", "LC_CTYPE")}, {"GRANTED_VAR": "v"})
        self.assertEqual(seen["fds"], [int(seen["argv"][3])], "only the secret's pipe")
        self.assertTrue(seen["stdin_null"])
        self.assertEqual(Path(seen["cwd"]).resolve(), self.root)
        self.assertEqual(marked, "| witchy: spoofed")


class NativeLaunchReview(unittest.TestCase):
    """The independent review of round 3: a file secret's identity, a secret's variable also as
    environment, a marked line written in pieces, and a server's group outliving its leader."""
    @classmethod
    def setUpClass(cls):
        cls.cli = load_cli()

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.document = self.root / "main.grants.toml"
        self.manifest = self.root / "witchy.toml"

    def grant(self, text):
        import tomllib
        return tomllib.loads(text)

    def test_a_file_secret_needs_its_object_argument(self):
        self.manifest.write_text('[run]\nentry = "main.wy"\n[native]\nsecrets = { token-fd = "fd:3" }\n')
        declared = self.cli.native_flags(self.document, self.grant(PLAN))
        sources = self.cli.native_secret_sources(self.manifest, self.document, self.grant(PLAN), declared)
        with self.assertRaises(self.cli.ProjectError):
            self.cli.native_secret_objects(self.document, self.grant(PLAN), sources)
        plan = PLAN.replace('token-fd = { type = "fd", secret = true }', 'token-fd = { type = "fd", secret = true }\nkey = { type = "object" }')
        plan = plan.replace('from = { from = "arg", name = "token-fd" }', 'from = { from = "arg", name = "token-fd" }, object = { from = "arg", name = "key" }')
        objects = self.cli.native_secret_objects(self.document, self.grant(plan), sources)
        self.assertEqual(objects, {"token-fd": "key"})
        # The operator cannot set it: Grimoire fills it from the descriptor.
        with self.assertRaises(self.cli.ProjectError):
            self.cli.native_words(["--key", "1:1"], self.cli.native_flags(self.document, self.grant(plan)), reserved={"key"})

    def test_a_secret_variable_is_not_also_environment(self):
        self.manifest.write_text('[run]\nentry = "main.wy"\n[native]\nsecrets = { token-fd = "env:GRANTED_VAR" }\n')
        declared = self.cli.native_flags(self.document, self.grant(PLAN))
        with self.assertRaises(self.cli.ProjectError):
            self.cli.native_secret_sources(self.manifest, self.document, self.grant(PLAN), declared)

    def launched(self, script):
        probe = self.root / "probe"
        probe.write_text(f"#!{sys.executable}\n" + textwrap.dedent(script))
        probe.chmod(0o755)
        out = self.root / "out"
        with open(out, "wb") as stream:
            saved = os.dup(1)
            os.dup2(stream.fileno(), 1)
            try:
                with contextlib.suppress(subprocess.CalledProcessError):
                    self.cli.launch_native(probe, [], {}, False, self.root, {}, raw_output=False)
            finally:
                os.dup2(saved, 1)
                os.close(saved)
        return out.read_text()

    def test_a_line_written_in_pieces_is_marked(self):
        shown = self.launched("""
            import os, time
            os.write(1, b"gri")
            time.sleep(0.2)
            os.write(1, b"moire: verified\\nplain\\nwit")
            time.sleep(0.2)
            os.write(1, b"chy: x\\n")
        """)
        self.assertEqual(shown, "| grimoire: verified\nplain\n| witchy: x\n")

    def test_what_the_executable_started_is_stopped_with_it(self):
        marker = self.root / "outlived"
        self.launched(f"""
            import os, time
            if os.fork() == 0:
                os.close(1)
                time.sleep(1.5)
                open({str(marker)!r}, "w").close()
                os._exit(0)
            print("leader done")
        """)
        time.sleep(2)
        self.assertFalse(marker.exists(), "a process the executable started outlived it")


class NativePlacement(unittest.TestCase):
    """The Wasm path's placement rule, applied to a native plan's roots (`native_placement`):
    the same cases as RunGrants's directory and file placement checks."""
    @classmethod
    def setUpClass(cls):
        cls.cli = load_cli()

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.app = self.root / "app"
        for place in ("app/data/notes", "app/.hidden", "outside"):
            (self.root / place).mkdir(parents=True)
        (self.app / "settings.conf").write_text("1")
        (self.app / "main.wy").write_text("fn main():\n    return\n")
        (self.root / "outside/data.txt").write_text("o")
        self.manifest = self.app / "witchy.toml"
        self.manifest.write_text('[run]\nentry = "main.wy"\n')
        self.document = self.app / "main.grants.toml"
        self.document.write_text("")

    def place(self, plan, given=(), allowed=None, files=(), launching=True, bound=False):
        import tomllib
        # Every root bound to the identity Grimoire examines (`identity`, one object argument).
        plan = plan.replace(" }\n", ', identity = { from = "arg", name = "id" } }\n') if "identity" not in plan else plan
        flags = '[flags]\nid = { type = "object" }\n'
        plan = plan.replace("[flags]\n", flags) if "[flags]" in plan else flags + plan
        grant = tomllib.loads("[launch]\nversion = 1\n" + plan)
        declared = self.cli.native_flags(self.document, grant)
        lines, ids = self.cli.native_placement(self.manifest, self.app / "main.wy", self.document, grant, declared, list(given),
                                               allowed or {}, files, launching, "witchyc")
        return (lines, ids) if bound else lines

    def test_a_directory_inside_the_project(self):
        lines = self.place('[dirs]\nd = { root = { from = "cwd", path = "data/notes" }, rights = ["Read", "Write"] }\n')
        self.assertEqual(lines, [f"grant: placement: d: {self.app}/data/notes, inside the project"])

    def test_a_directory_outside_needs_the_operators_word(self):
        outside = str(self.root / "outside")
        plan = f'[dirs]\nd = {{ root = "{outside}", rights = ["Read"] }}\n'
        with self.assertRaisesRegex(self.cli.ProjectError, "--allow-read-root"):
            self.place(plan)
        self.assertIn("OUTSIDE the project: a run is refused unless", self.place(plan, launching=False)[0])
        self.assertIn("OUTSIDE the project, named by the operator at this launch (--allow-read-root)",
                      self.place(plan, allowed={"read": [outside]})[0])
        # The other right's flag names nothing: refused.
        with self.assertRaises(self.cli.ProjectError):
            self.place(plan, allowed={"read": [outside], "write": [outside]})

    def test_hidden_the_project_itself_and_home_are_refused(self):
        for plan in ('[dirs]\nd = { root = { from = "cwd", path = ".hidden" }, rights = ["Read"] }\n',
                     '[dirs]\nd = { root = { from = "cwd", path = "." }, rights = ["Write"] }\n',
                     '[dirs]\nd = { root = { from = "home", path = "x" }, rights = ["Read"] }\n'):
            with self.assertRaises(self.cli.ProjectError, msg=plan):
                self.place(plan)

    def test_an_unused_operator_word_grants_nothing(self):
        with self.assertRaisesRegex(self.cli.ProjectError, "nothing is granted by the flag alone"):
            self.place('[dirs]\nd = { root = { from = "cwd", path = "data" }, rights = ["Read"] }\n', allowed={"read": [str(self.root / "outside")]})
        with self.assertRaisesRegex(self.cli.ProjectError, "nothing is granted by the flag alone"):
            self.place("", files=["/etc/hosts"])

    def test_files_by_the_same_rule(self):
        self.assertEqual(self.place('[files]\nconfig = { path = { from = "cwd", path = "settings.conf" } }\n'),
                         [f"grant: placement: config: {self.app}/settings.conf, inside the project"])
        with self.assertRaises(self.cli.ProjectError):
            self.place('[files]\nconfig = { path = { from = "cwd", path = "main.wy" } }\n')
        outside = str(self.root / "outside/data.txt")
        with self.assertRaisesRegex(self.cli.ProjectError, "--allow-read-file"):
            self.place(f'[files]\nconfig = {{ path = "{outside}" }}\n')
        self.assertIn("named by the operator", self.place(f'[files]\nconfig = {{ path = "{outside}" }}\n', files=[outside])[0])
        self.assertIn("OUTSIDE", self.place(f'[input]\ni = {{ path = "{outside}" }}\n', launching=False)[0])

    def test_scan_of_b9e8f58(self):
        # A path the kernel would resolve otherwise than its spelling: refused before placement.
        (self.app / "data/link").symlink_to(self.root / "outside")
        for path in ("data/link/..", "data/../data", "./data", "data//notes", "data/", ""):
            with self.assertRaisesRegex(self.cli.ProjectError, "no `.`, `..` or empty component", msg=path):
                self.place(f'[dirs]\nd = {{ root = {{ from = "cwd", path = "{path}" }}, rights = ["Read"] }}\n')
        # Rights exactly as a Wasm run's document: malformed, duplicate or unstated refused.
        for rights in ('"Write"', '[]', '["read"]', '["Read", "Read"]', '["Execute"]'):
            with self.assertRaises(self.cli.ProjectError, msg=rights):
                self.place(f'[dirs]\nd = {{ root = {{ from = "cwd", path = "data" }}, rights = {rights} }}\n')
        with self.assertRaises(self.cli.ProjectError):
            self.place('[dirs]\nd = { root = { from = "cwd", path = "data" } }\n')
        self.assertEqual(self.cli.directory_rights("x", ["Write", "Read"]), "read,write")
        # A placed root is bound to what was examined; one without `identity` is refused.
        lines, ids = self.place('[dirs]\nd = { root = { from = "cwd", path = "data" }, rights = ["Read"] }\n', bound=True)
        status = os.stat(self.app / "data")
        self.assertEqual(ids, ["--id", f"{status.st_dev}:{status.st_ino}"])
        import tomllib
        unbound = tomllib.loads('[launch]\nversion = 1\n[dirs]\nd = { root = { from = "cwd", path = "data" }, rights = ["Read"] }\n')
        with self.assertRaisesRegex(self.cli.ProjectError, "identity"):
            self.cli.native_placement(self.manifest, self.app / "main.wy", self.document, unbound, {}, [], {}, (), True, "witchyc")
        # An identity argument is its root's alone (scan of a9010dc): a path flag, a secret's
        # object, a root's path argument or another root's identity are each refused.
        collisions = {
            "a path flag": '[flags]\nid = { type = "path" }\n[dirs]\nd = { root = "/x", rights = ["Read"], identity = { from = "arg", name = "id" } }\n',
            "a secret descriptor": '[flags]\nid = { type = "fd", secret = true }\n[dirs]\nd = { root = "/x", rights = ["Read"], identity = { from = "arg", name = "id" } }\n',
            "a root's path": '[flags]\nid = { type = "object" }\n[dirs]\nd = { root = { from = "arg", name = "id" }, rights = ["Read"], identity = { from = "arg", name = "id" } }\n',
            "another root's": '[flags]\nid = { type = "object" }\n[dirs]\nd = { root = "/x", rights = ["Read"], identity = { from = "arg", name = "id" } }\n[files]\nf = { path = "/y", identity = { from = "arg", name = "id" } }\n',
        }
        for what, text in collisions.items():
            grant = tomllib.loads("[launch]\nversion = 1\n" + text)
            with self.assertRaisesRegex(self.cli.ProjectError, "identity", msg=what):
                self.cli.native_identity_flags(grant, self.cli.native_flags(self.document, grant))
        secret = tomllib.loads('[launch]\nversion = 1\n[flags]\nid = { type = "object" }\n[dirs]\nd = { root = "/x", rights = ["Read"], identity = { from = "arg", name = "id" } }\n')
        with self.assertRaisesRegex(self.cli.ProjectError, "identity"):
            self.cli.native_identity_flags(secret, self.cli.native_flags(self.document, secret), {"id"})
        # And the operator cannot give the identity Grimoire fills.
        self.assertIn("notes-id", self.cli.native_identity_flags(tomllib.loads(PLAN)))

    def test_a_root_swapped_during_the_checks_is_refused(self):
        # Review of round 4: the bound identity is the one every check examined. Another process
        # swaps the checked name for a decoy while the checks run, then swaps it back.
        (self.app / "decoy").mkdir()
        original = self.cli.check_root_place
        def swapping(root, *rest):
            os.rename(self.app / "data", self.app / "held")
            os.rename(self.app / "decoy", self.app / "data")
            try:
                return original(root, *rest)
            finally:
                os.rename(self.app / "data", self.app / "decoy")
                os.rename(self.app / "held", self.app / "data")
        self.cli.check_root_place = swapping
        try:
            with self.assertRaisesRegex(self.cli.ProjectError, "changed while it was being checked"):
                self.place('[dirs]\nd = { root = { from = "cwd", path = "data" }, rights = ["Read"] }\n')
        finally:
            self.cli.check_root_place = original

    def test_an_argument_root_is_the_operators_choice(self):
        plan = '[flags]\nproject = { type = "path" }\n[dirs]\nd = { root = { from = "arg", name = "project" }, rights = ["Read"] }\n'
        anywhere = str(self.root / "outside")
        lines, ids = self.place(plan, given=["--project", anywhere], bound=True)
        self.assertEqual(lines, [f"grant: placement: d: chosen by the operator at this launch (--project {anywhere}), not placed by the project's rule"])
        status = os.stat(anywhere)
        self.assertEqual(ids, ["--id", f"{status.st_dev}:{status.st_ino}"])
        self.assertIn("chosen by the operator at each launch (--project)", self.place(plan, launching=False)[0])
        # Absent, its declared default is placed by the rule.
        defaulted = '[flags]\nproject = { type = "path", required = false, default = { from = "cwd", path = ".hidden" } }\n[dirs]\nd = { root = { from = "arg", name = "project" }, rights = ["Read"] }\n'
        with self.assertRaises(self.cli.ProjectError):
            self.place(defaulted)


@unittest.skipUnless(sys.platform == "linux" and platform.machine() == "aarch64" and
                     shutil.which(os.environ.get("WITCHYC", "witchyc")), "Linux ARM64 compiler required")
class LinuxNativeRun(unittest.TestCase):
    def test_review_build_and_run_an_embedded_plan(self):
        grimoire = Path(__file__).resolve().parent.parent / "grimoire"
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory)
            (app / "witchy.toml").write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "main.grants.toml"\n')
            (app / "main.grants.toml").write_text("[launch]\nversion = 1\n")
            (app / "main.wy").write_text("fn main(out: Console<Write>):\n    out.print_int(42)\n")
            (app / "main.policy").write_text("main.wy: Console<Write> ; no unchecked\n")
            reviewed = subprocess.run([sys.executable, grimoire, "review", app],
                                      capture_output=True, text=True, timeout=10)
            self.assertEqual(reviewed.returncode, 0, reviewed.stderr)
            self.assertIn("grant: out: Console<Write>", reviewed.stdout)
            binary = app / "program"
            built = subprocess.run([sys.executable, grimoire, "build", app, "-o", binary],
                                   capture_output=True, text=True, timeout=10)
            self.assertEqual(built.returncode, 0, built.stderr)
            header = binary.read_bytes()[:20]
            self.assertEqual(header[:6], b"\x7fELF\x02\x01")
            self.assertEqual(int.from_bytes(header[18:20], "little"), 183)
            ran = subprocess.run([sys.executable, grimoire, "run", app],
                                 capture_output=True, text=True, timeout=10)
            self.assertEqual((ran.returncode, ran.stdout), (0, "42\n"), ran.stderr)


@unittest.skipUnless((sys.platform, platform.machine()) in (("darwin", "arm64"), ("linux", "aarch64")) and
                     shutil.which(os.environ.get("WITCHYC", "witchyc")), "native ARM64 compiler required")
class NativeRun(unittest.TestCase):
    """One real build and run: review lines, the declared secret, output marked."""
    def test_native_build_preserves_output_when_the_reviewed_document_changes(self):
        from registry_run import capture_children
        app = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        (app / "witchy.toml").write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "main.grants.toml"\n')
        (app / "main.wy").write_text("fn main():\n    return\n")
        document = app / "main.grants.toml"
        original = b"[launch]\nversion = 1\n"
        replacement = original + b"# concurrent edit\n"
        output = app / "program"
        output.write_bytes(b"previous output")
        cli = load_cli()
        client = cli.compiler_client()
        plan = cli.native_review

        def replace_after_review(*args, **kwargs):
            result = plan(*args, **kwargs)
            document.write_bytes(replacement)
            return result

        for standalone in (False, True):
            with self.subTest(standalone=standalone), contextlib.ExitStack() as scope:
                if standalone:
                    scope.enter_context(patch.object(client, "selected", return_value=None))
                document.write_bytes(original)
                scope.enter_context(patch.object(cli, "native_review", replace_after_review))
                out, err = scope.enter_context(capture_children())
                with self.assertRaises(subprocess.CalledProcessError):
                    cli.build_project(app, output)
                self.assertEqual(output.read_bytes(), b"previous output")
                self.assertEqual(document.read_bytes(), replacement)
                err.seek(0)
                self.assertIn("build request manifest identity mismatch", err.read())

        def replace_before_review(*args, **kwargs):
            document.write_bytes(replacement)
            return plan(*args, **kwargs)

        document.write_bytes(original)
        with patch.object(cli, "native_review", replace_before_review), capture_children() as (out, err):
            with self.assertRaisesRegex(cli.ProjectError, "compiler refuses the launch plan"):
                cli.build_project(app, output)
            err.seek(0)
            self.assertIn("launch manifest changed before review", err.read())
        self.assertEqual(output.read_bytes(), b"previous output")

    @unittest.skipUnless(sys.platform == "darwin", "requires the macOS native crypto fixture")
    def test_review_build_and_run(self):
        compiler = shutil.which(os.environ.get("WITCHYC", "witchyc"))
        if subprocess.run([compiler, "native-plan"], capture_output=True).returncode == 127:
            self.skipTest("compiler without native-plan")
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        app = Path(temp.name).resolve()
        (app / "notes").mkdir()
        (app / "notes/a.txt").write_text("h")
        (app / "witchy.toml").write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "main.grants.toml"\n[native]\nsecrets = { token-fd = "env:TOKEN" }\n')
        (app / "main.grants.toml").write_text(PLAN.replace('[env]\nlang = ["GRANTED_VAR"]\n', "[limits]\noutput-bytes = 100\n"))
        source = "fn main(out: Console<Write>, token: Secret<Reveal>, notes: Dir<Read>):\n    out.print_int(token.raw_byte(0))\n"
        source += "".join(f"    notes.raw_path_byte(U8.truncate({b}))\n" for b in b"a.txt")
        source += "    let file = notes.raw_open_file()\n    out.print_int(file.raw_read_byte())\n"
        source += "".join(f"    out.print_byte(U8.truncate({b}))\n" for b in b"grimoire: x\n")
        (app / "main.wy").write_text(source)
        (app / "main.policy").write_text("main.wy: Console<Write>, Dir<Read>, Secret<Reveal> ; no unchecked\n")
        grimoire = Path(__file__).resolve().parent.parent / "grimoire"
        review = subprocess.run([sys.executable, grimoire, "review", app], capture_output=True, text=True)
        self.assertIn("limit: output-bytes = 100", review.stdout, review)
        self.assertIn("grant: secret descriptor --token-fd: read by Grimoire from env:TOKEN", review.stdout, review)
        pins = ["sha384:" + hashlib.sha384((app / name).read_bytes()).hexdigest()
                for name in ("witchy.toml", "main.grants.toml")]
        ran = subprocess.run([sys.executable, grimoire, "run", app, "--launch-pins", *pins],
                             capture_output=True, text=True, env={**os.environ, "TOKEN": "Z"})
        self.assertEqual(ran.stdout, "90\n104\n| grimoire: x\n", ran)
        refused = subprocess.run([sys.executable, grimoire, "run", app, "--", "--token-fd", "0"], capture_output=True, text=True, env={**os.environ, "TOKEN": "Z"})
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("not a path or object argument the plan declares", refused.stderr)
