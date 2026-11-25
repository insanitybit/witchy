"""Fixed secrets through `grimoire run`, with canaries: the configured witchyc and
witchy-wasmtime-host (from Witchy 2 d476170d or later), and COVEN for the registry check.

Signing needs a proof checker. Set WITCHY_WASM_READY_BINDING or WITCHY_WASM_LEAN_ROOT.
With a ready binding, WITCHY_PROOF_CACHE can speed up repeated checks.

Every toolchain step and the engine run through a wrapper that records, before it starts the
real tool, its environment, its arguments, the descriptors it inherited (and, for a toolchain
step, what they hold), and its parent's environment as another process of the same user reads
it (`ps -E` on macOS, /proc/PID/environ on Linux). No canary may appear in any of it."""

import hashlib
import hmac
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
ALPHA, BETA = b"canary-a1pha-5e3d", b"canary-b3ta-9c1f"
# A store's secrets (`main(store: SecretStore)`): a signing key and a token it may read.
SIGNING, TOKEN = b"canary-s1gn-st0re", b"canary-t0ken-st0re"
CANARIES = ("canary-a1pha-5e3d", "canary-b3ta-9c1f", "canary-s1gn-st0re", "canary-t0ken-st0re")
ALPHA_MESSAGE, BETA_MESSAGE = "what do ya want for nothing?", "The quick brown fox jumps over the lazy dog"

PROGRAM = f"""import console
import secret

fn endorse(console: &Console<Write>, allocator: &Allocator, key: &Secret<Seal>, message: Text):
    match key.sign(allocator, message):
        Ok(signature):
            console.write_string(&signature)
            console.line("")
            signature.close()
        Err(e):
            console.line("unsigned")

fn main(beta: Secret<Seal>, console: Console<Write>, allocator: Allocator, alpha: Secret<Seal>):
    endorse(&console, &allocator, &alpha, "{ALPHA_MESSAGE}")
    endorse(&console, &allocator, &beta, "{BETA_MESSAGE}")
"""

# The store's view: the key signs and is never a full `Secret`; the token is read (its length
# only, never printed) and signs; a name the document does not grant is not there.
STORE_PROGRAM = f"""import console
import secret

fn endorse(console: &Console<Write>, allocator: &Allocator, key: &Secret<Seal>, message: Text):
    match key.sign(allocator, message):
        Ok(signature):
            console.write_string(&signature)
            console.line("")
            signature.close()
        Err(e):
            console.line("unsigned")

fn main(console: Console<Write>, allocator: Allocator, store: SecretStore):
    match store.get_sealed("GRIMOIRE_TEST_SIGNING"):
        Ok(key):
            endorse(&console, &allocator, &key, "{ALPHA_MESSAGE}")
        Err(e):
            console.line("no key")
    match store.get("GRIMOIRE_TEST_SIGNING"):
        Ok(key):
            console.line("leaked")
        Err(SealedOnly):
            console.line("sealed only")
        Err(_):
            console.line("no key")
    match store.get("GRIMOIRE_TEST_TOKEN"):
        Ok(readable):
            match readable.reveal(&allocator):
                Ok(text):
                    console.print_int(text.byte_len())
                    text.close()
                Err(_):
                    console.line("unreadable")
            endorse(&console, &allocator, &readable, "{BETA_MESSAGE}")
        Err(e):
            console.line("no token")
    match store.get("PATH"):
        Ok(path):
            console.line("leaked")
        Err(e):
            console.line("not granted")
"""
STORE_DOCUMENT = '[secrets]\nstore = { GRIMOIRE_TEST_SIGNING = "seal", GRIMOIRE_TEST_TOKEN = "reveal" }\n'

# Runs as the tool it stands for: records what it was given, then becomes the real tool with the
# same descriptors. `{real}`, `{seen}` and `{reads}` are filled in per wrapper.
WRAPPER = """#!{python} -I
import json, os, subprocess, sys
mine = []
for name in os.listdir("/dev/fd"):
    try:
        if int(name) >= 3:
            os.fstat(int(name))
            mine.append(int(name))
    except (OSError, ValueError):
        pass
held = {{}}
if {reads}:
    for number in mine:
        try:
            held[number] = os.pread(number, 1 << 16, 0).decode("latin-1")
        except OSError:
            try:
                held[number] = os.read(number, 1 << 16).decode("latin-1")
            except OSError:
                held[number] = None
parent = os.getppid()
try:
    with open(f"/proc/{{parent}}/environ", "rb") as stream:
        environ = stream.read().decode("latin-1")
except OSError:
    environ = subprocess.run(["/bin/ps", "-E", "-ww", "-o", "command=", "-p", str(parent)], capture_output=True, text=True, errors="replace").stdout
parent_fds = []
if {inspect_fds}:
    try:
        parent_fds = sorted(int(n) for n in os.listdir(f"/proc/{{parent}}/fd"))
    except OSError:
        listed = subprocess.run(["/usr/sbin/lsof", "-p", str(parent), "-a", "-d", "0-1023", "-Ff"], capture_output=True, text=True).stdout
        parent_fds = sorted(int(line[1:]) for line in listed.splitlines() if line[:1] == "f" and line[1:].isdigit())
with open({seen!r}, "a") as out:
    out.write(json.dumps({{"tool": {tool!r}, "argv": sys.argv, "env": dict(os.environ), "descriptors": sorted(mine),
                          "held": held, "parent": environ, "parent_fds": parent_fds}}) + "\\n")
os.execv({real!r}, [{real!r}, *sys.argv[1:]])
"""


# Run in process in the default suite (`SecretRun.grimoire`); each has a twin of the same body
# through the launched `grimoire`, run with --process-boundaries.
AFTER_THE_COMPILER_IN_PROCESS = {
    "test_a_registry_dependency_cannot_supply_or_widen_a_secret_grant",
    "test_a_file_grant_alias_is_refused_with_only_a_store",
    "test_a_store_no_parameter_takes_is_refused",
}


def tag(key, message):
    return hmac.new(key, message.encode(), hashlib.sha256).hexdigest()


class SecretRun(unittest.TestCase):
    def setUp(self):
        if not (os.environ.get("WITCHY_WASM_READY_BINDING") or os.environ.get("WITCHY_WASM_LEAN_ROOT")):
            self.fail("signing needs a proof checker: set WITCHY_WASM_READY_BINDING or WITCHY_WASM_LEAN_ROOT")
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.app = self.root / "app"
        self.app.mkdir()
        (self.app / "witchy.toml").write_text('[run]\nentry = "main.wy"\n\n[grants]\ndocument = "main.grants.toml"\noutput = true\nallocator = true\n')
        (self.app / "main.wy").write_text(PROGRAM)
        self.declare()
        self.seen = self.root / "seen.jsonl"
        self.tools = {}
        for variable, reads in [("WITCHYC", True), ("WITCHY_WASMTIME_HOST", False), ("COVEN", True)]:
            if variable not in os.environ:
                continue
            real = shutil.which(os.environ[variable])
            wrapper = self.root / f"wrapper-{variable.lower()}"
            # The engine's descriptors are its secrets' pipes: it is not read here, only listed.
            wrapper.write_text(WRAPPER.format(python=sys.executable, real=real, seen=str(self.seen), reads=reads, tool=variable,
                                              inspect_fds=variable == "WITCHY_WASMTIME_HOST" and
                                              self._testMethodName == "test_a_pipe_sources_identity_goes_to_the_engine_and_stays_live"))
            wrapper.chmod(0o755)
            self.tools[variable] = str(wrapper)

    def declare(self, alpha_also=(), extra=""):
        also = "".join(f', also_from = ["{source}"]' for source in alpha_also[:1])
        (self.app / "main.grants.toml").write_text(
            "[secrets]\n"
            f'alpha = {{ use = "seal", from = "env:GRIMOIRE_TEST_ALPHA"{also} }}\n'
            'beta = { use = "seal", from = "env:GRIMOIRE_TEST_BETA" }\n' + extra)

    def grimoire(self, *args, variables=None, pass_fds=(), cwd=None):
        # Argument and grant rules that must refuse before any child starts.
        # The process-boundary cases below separately inspect actual children.
        if self._testMethodName in {
            "test_a_secret_named_like_a_toolchain_variable_is_refused",
            "test_a_missing_store_variable_is_refused_before_the_build",
            "test_bind_to_an_undeclared_source_is_refused_before_the_build",
            "test_two_descriptors_on_one_file_are_refused_before_the_build",
            "test_a_missing_input_is_refused_before_the_build_without_fallback",
            "test_a_store_secret_on_a_second_route_is_refused_before_the_build",
            "test_a_store_entry_that_is_not_a_use_is_refused_before_the_build",
        }:
            import contextlib
            import io
            from unittest.mock import patch
            from registry_run import load_cli
            cli = load_cli()
            out, err = io.StringIO(), io.StringIO()
            with contextlib.chdir(cwd or self.root), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), \
                 patch.object(cli, "INHERITED_DESCRIPTORS", frozenset(pass_fds)), \
                 patch.object(cli.subprocess, "Popen", side_effect=AssertionError("a tool started before refusal")):
                rc = cli.cli_main(list(args), variables or {})
            return subprocess.CompletedProcess(args, rc, out.getvalue(), err.getvalue())
        if self._testMethodName in AFTER_THE_COMPILER_IN_PROCESS:
            # Refusals that come after the compiler's step: Grimoire's command in this process,
            # with the real tools through the same recording wrappers, the secrets as the launch
            # gives them and `pass_fds` as the descriptors it inherited. The same case through
            # a launched `grimoire` is its `_through_the_launcher` twin (--process-boundaries).
            import contextlib
            from unittest.mock import patch
            from registry_run import load_cli, capture_children
            cli = load_cli()
            with contextlib.chdir(cwd or self.root), patch.dict(os.environ, self.tools), capture_children() as (out, err), \
                 patch.object(cli, "INHERITED_DESCRIPTORS", frozenset(pass_fds)):
                rc = cli.cli_main(list(map(str, args)), variables or {})
                out.seek(0)
                err.seek(0)
                return subprocess.CompletedProcess(args, rc, out.read(), err.read())
        env = dict(os.environ, **self.tools, **(variables or {}))
        return subprocess.run([ROOT / "grimoire", *args], cwd=cwd or self.root, capture_output=True, text=True,
                              timeout=120, env=env, pass_fds=pass_fds)

    def records(self):
        return [json.loads(line) for line in self.seen.read_text().splitlines()] if self.seen.exists() else []

    def assert_no_canary(self, result):
        for canary in CANARIES:
            self.assertNotIn(canary, result.stdout + result.stderr)
            for record in self.records():
                self.assertNotIn(canary, json.dumps(record), f"{record['tool']} saw a canary")

    def assert_engine_got_only_its_pipes(self, slots):
        engines = [r for r in self.records() if r["tool"] == "WITCHY_WASMTIME_HOST"]
        self.assertEqual(len(engines), 1)
        engine = engines[0]
        # Grimoire starts it with none; the wrapper's own interpreter adds its locale and, on
        # macOS, CoreFoundation's text encoding before it records.
        self.assertLessEqual(set(engine["env"]), {"LC_CTYPE", "__CF_USER_TEXT_ENCODING"})
        flags = [a for a in engine["argv"] if a.startswith("--secret-slot=")]
        self.assertEqual(sorted(f.split(":")[0] for f in flags), sorted(f"--secret-slot={s}" for s in slots))
        pipes = sorted(int(f.rsplit("fd:", 1)[1]) for f in flags)
        self.assertTrue(all(":fd:" in f for f in flags), flags)
        self.assertEqual(engine["descriptors"], sorted(pipes + [module_descriptor(engine), lifeline_descriptor(engine)]))

    def test_two_sealed_slots_sign_with_their_own_keys_and_no_step_sees_a_value(self):
        result = self.grimoire("run", "app", variables={"GRIMOIRE_TEST_ALPHA": ALPHA.decode(), "GRIMOIRE_TEST_BETA": BETA.decode()})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"{tag(ALPHA, ALPHA_MESSAGE)}\n{tag(BETA, BETA_MESSAGE)}\n")
        self.assertEqual({r["tool"] for r in self.records()}, {"WITCHYC", "WITCHY_WASMTIME_HOST"})
        self.assert_no_canary(result)
        self.assert_engine_got_only_its_pipes(["alpha", "beta"])
        # The toolchain steps inherit no descriptor at all.
        self.assertTrue(all(r["descriptors"] == [] for r in self.records() if r["tool"] != "WITCHY_WASMTIME_HOST"))

    def test_a_secret_named_like_a_toolchain_variable_is_refused(self):
        """Launcher configuration can reach a toolchain step, so it cannot also supply a secret.
        The rest of WITCHY_ is reserved for the toolchain, though no step is given it."""
        for variable in ("WITCHY_PROOF_JOBS", "SSL_CERT_FILE"):
            with self.subTest(variable=variable):
                (self.app / "main.grants.toml").write_text(
                    f'[secrets]\nalpha = {{ use = "seal", from = "env:{variable}" }}\n'
                    'beta = { use = "seal", from = "env:PATH" }\n')
                result = self.grimoire("run", "app", variables={variable: ALPHA.decode()})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"secret slot alpha: source env:{variable} is a variable the toolchain is given", result.stderr)
                self.assertEqual(self.records(), [])
                self.assert_no_canary(result)
        (self.app / "main.grants.toml").write_text('[secrets]\nalpha = { use = "seal", from = "env:WITCHY_ALPHA" }\n'
                                                   'beta = { use = "seal", from = "env:GRIMOIRE_TEST_BETA" }\n')
        result = self.grimoire("run", "app", variables={"WITCHY_ALPHA": ALPHA.decode()})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("secret slot alpha: source env:WITCHY_ALPHA is a WITCHY_ variable, reserved for the toolchain", result.stderr)
        self.assertEqual(self.records(), [])
        self.assert_no_canary(result)

    def key_file(self):
        """A key file in the app, with ALPHA, and two descriptors open on it."""
        key = self.app / "key.txt"
        key.write_bytes(ALPHA)
        first, second = os.open(key, os.O_RDONLY), os.open(key, os.O_RDONLY)
        self.addCleanup(lambda: [os.close(fd) for fd in (first, second) if fd_open(fd)])
        return key, first, second

    def test_two_descriptors_on_one_file_are_refused_before_the_build(self):
        """One source, one route: the same file opened twice is one source, whatever the numbers
        (review of 33face12: a sealed key read back through a second slot)."""
        _, first, second = self.key_file()
        (self.app / "main.grants.toml").write_text(
            "[secrets]\n"
            f'alpha = {{ use = "seal", from = "env:GRIMOIRE_TEST_ALPHA", also_from = ["fd:{first}"] }}\n'
            f'beta = {{ use = "seal", from = "env:GRIMOIRE_TEST_BETA", also_from = ["fd:{second}"] }}\n')
        result = self.grimoire("run", "app", "--bind", f"alpha=fd:{first}", "--bind", f"beta=fd:{second}", pass_fds=(first, second))
        self.refused_before_the_build(result, f"secret slot beta: source is the same file or pipe as slot alpha's (fd:{second})")

    def test_a_file_grant_that_is_a_secret_source_is_refused(self):
        """A `[files]` grant naming the key's file, or a hard link to it, would give the program
        the key as an ordinary file (review of 33face12). Refused before the engine starts."""
        from registry_run import load_cli
        cli = load_cli()
        key, first, _ = self.key_file()
        linked = self.app / "other.txt"
        os.link(key, linked)
        chosen = {"alpha": f"fd:{first}"}
        from unittest.mock import patch
        with patch.object(cli, "INHERITED_DESCRIPTORS", frozenset([first])):
            objects = cli.source_objects(chosen)
        for path in (key, linked):
            with self.subTest(path=path), self.assertRaises(cli.SecretError) as raised:
                cli.check_granted_file(str(path), True, objects, chosen)
            self.assertIn(f"secret slot alpha: source is also granted as a file (fd:{first})", str(raised.exception))
            self.assertNotIn(ALPHA.decode(), str(raised.exception))
        # A genuinely distinct regular file is allowed.
        other = self.app / "settings"
        other.touch()
        cli.check_granted_file(str(other), True, objects, chosen)

    def test_the_engine_refuses_a_source_file_linked_in_after_grimoires_check(self):
        """Grimoire's `[files]` check is a pre-check: the granted name can become the key's file
        after it. Here the compiler's build step (after the check, before the engine starts)
        replaces the granted file with a hard link to the key. The engine reads the key from a
        pipe, so only `--secret-object` tells it which file the pipe stands for: it refuses."""
        key, first, _ = self.key_file()
        config = self.app / "settings.conf"
        config.write_text("plain settings\n")
        (self.app / "main.wy").write_text(PROGRAM.replace("import secret\n", "import secret\nimport fs\n").replace(
            "alpha: Secret<Seal>):\n", "alpha: Secret<Seal>, config: File<Read>):\n"
            "    match config.read_text(&allocator):\n        Ok(text):\n            console.write_string(&text)\n"
            "            console.line(\"\")\n            text.close()\n        Err(e):\n            console.line(\"unreadable\")\n"))
        self.declare(alpha_also=[f"fd:{first}"], extra='\n[files]\nconfig = "settings.conf"\n')
        swap = self.root / "wrapper-swap"
        inner = self.tools["WITCHYC"]
        swap.write_text(f"#!{sys.executable} -I\nimport os, sys\nif sys.argv[1:2] == ['build']:\n"
                        f"    os.unlink({str(config)!r})\n    os.link({str(key)!r}, {str(config)!r})\n"
                        f"os.execv({inner!r}, [{inner!r}, *sys.argv[1:]])\n")
        swap.chmod(0o755)
        self.tools["WITCHYC"] = str(swap)
        result = self.grimoire("run", "app", "--bind", f"alpha=fd:{first}", pass_fds=(first,),
                               variables={"GRIMOIRE_TEST_BETA": BETA.decode()})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("invalid grant: --file is a secret slot's source", result.stderr)
        self.assertEqual(os.stat(config).st_ino, os.stat(key).st_ino, "the swap happened")
        self.assert_no_canary(result)
        # The key's file is named to the engine by identity, once, for alpha only: beta's value
        # came from a variable, which has no file.
        status = os.stat(key)
        engine = [r for r in self.records() if r["tool"] == "WITCHY_WASMTIME_HOST"]
        self.assertEqual(len(engine), 1)
        objects = [a for a in engine[0]["argv"] if a.startswith("--secret-object=")]
        self.assertEqual(objects, [f"--secret-object=alpha:{status.st_dev & 0xFFFF_FFFF_FFFF_FFFF}:{status.st_ino}"])

    def test_a_pipe_sources_identity_goes_to_the_engine_and_stays_live(self):
        """Every descriptor source's identity is passed on, a pipe's too: a named FIFO cannot be
        told from a pipe, and the flag only adds refusals (review of d7b03ec1)."""
        reading, writing = os.pipe()
        self.addCleanup(lambda: [os.close(fd) for fd in (reading, writing) if fd_open(fd)])
        self.declare(alpha_also=[f"fd:{reading}"])
        os.write(writing, ALPHA)
        os.close(writing)
        result = self.grimoire("run", "app", "--bind", f"alpha=fd:{reading}", pass_fds=(reading,),
                               variables={"GRIMOIRE_TEST_ALPHA": "canary-a1pha-5e3d-unused", "GRIMOIRE_TEST_BETA": BETA.decode()})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"{tag(ALPHA, ALPHA_MESSAGE)}\n{tag(BETA, BETA_MESSAGE)}\n")
        self.assert_no_canary(result)
        self.assert_engine_got_only_its_pipes(["alpha", "beta"])
        engine = [r for r in self.records() if r["tool"] == "WITCHY_WASMTIME_HOST"]
        status = os.fstat(reading)
        self.assertEqual([a for a in engine[0]["argv"] if a.startswith("--secret-object=")],
                         [f"--secret-object=alpha:{status.st_dev & 0xFFFF_FFFF_FFFF_FFFF}:{status.st_ino}"])
        # Grimoire still holds the source while the engine runs, so the pipe, and the identity the
        # engine was given, stay live: a freed pipe's inode number is soon reused on macOS and
        # could match another slot's fresh pipe (review of 925bb2d).
        self.assertIn(reading, engine[0]["parent_fds"])

    def test_selected_regular_file_supplies_the_slot(self):
        from registry_run import load_cli
        from unittest.mock import patch
        cli = load_cli()
        key, _, _ = self.key_file()
        with key.open("rb") as stream:
            fd = stream.fileno()
            source = f"fd:{fd}"
            self.declare(alpha_also=[source])
            declared = cli.read_secret_slots(self.app / "main.grants.toml")
            with patch.object(cli, "INHERITED_DESCRIPTORS", frozenset([fd])):
                chosen = cli.select_secret_sources(declared, [f"alpha={source}"])
                self.assertEqual(cli.source_objects(chosen)[(key.stat().st_dev, key.stat().st_ino)], "alpha")
                values = cli.resolve_secrets(declared, chosen, {"GRIMOIRE_TEST_BETA": BETA})
                self.assertEqual(values["alpha"], ALPHA)
                self.assertEqual(values["beta"], BETA)

    def test_missing_file_and_descriptor_alias_refusals_with_secrets(self):
        from registry_run import load_cli
        from unittest.mock import patch
        cli = load_cli()
        _, fd, _ = self.key_file()
        for chosen in ({}, {"alpha": f"fd:{fd}"}):
            with patch.object(cli, "INHERITED_DESCRIPTORS", frozenset([fd])):
                objects = cli.source_objects(chosen)
            for path, message in ((self.app / "missing.txt", "does not exist"), (f"/dev//fd/{fd}", "is a descriptor alias")):
                with self.subTest(chosen=chosen, path=path), self.assertRaisesRegex(cli.ProjectError, message) as raised:
                    cli.check_granted_file(str(path), True, objects, chosen)
                self.assertNotIn(ALPHA.decode(), str(raised.exception))

    def test_a_file_grant_that_is_not_a_regular_file_is_refused_with_secrets(self):
        from registry_run import load_cli
        cli = load_cli()
        # A differently spelled device on a case-insensitive volume and a character device.
        # On a case-sensitive volume use a symlink to the device to exercise realpath/stat.
        linked = self.app / "device"
        linked.symlink_to("/dev/null")
        for path in ("/dev/null", "/dEv/fd/0" if os.path.exists("/dEv/fd/0") else str(linked)):
            with self.subTest(path=path), self.assertRaises(cli.ProjectError) as raised:
                cli.check_granted_file(path, True, {}, {})
            self.assertRegex(str(raised.exception), "not a regular file|descriptor alias")


    def refused_before_the_build(self, result, message):
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn(message, result.stderr)
        self.assertEqual(self.records(), [], "nothing was fetched, checked or built")
        self.assert_no_canary(result)

    def test_bind_to_an_undeclared_source_is_refused_before_the_build(self):
        reading, writing = os.pipe()
        self.addCleanup(lambda: [os.close(fd) for fd in (reading, writing) if fd_open(fd)])
        os.write(writing, ALPHA)
        os.close(writing)
        variables = {"GRIMOIRE_TEST_ALPHA": ALPHA.decode(), "GRIMOIRE_TEST_BETA": BETA.decode()}
        # An undeclared descriptor, even one that is open and holds a value: no fallback to `from`.
        self.refused_before_the_build(self.grimoire("run", "app", "--bind", f"alpha=fd:{reading}", pass_fds=(reading,), variables=variables),
                                      f"secret slot alpha: source not declared (neither from nor also_from) (fd:{reading})")
        self.refused_before_the_build(self.grimoire("run", "app", "--bind", "alpha=env:GRIMOIRE_TEST_BETA", variables=variables),
                                      "secret slot alpha: source not declared")
        self.refused_before_the_build(self.grimoire("run", "app", "--bind", "gamma=env:GRIMOIRE_TEST_ALPHA", variables=variables),
                                      "secret slot gamma: not declared in the grant document's [secrets]")
        self.refused_before_the_build(self.grimoire("run", "app", "--bind", "alpha=env:GRIMOIRE_TEST_ALPHA",
                                                    "--bind", "alpha=env:GRIMOIRE_TEST_ALPHA", variables=variables),
                                      "secret slot alpha: selected twice")
        # A value given where a source belongs is not repeated.
        result = self.grimoire("run", "app", "--bind", f"alpha={ALPHA.decode()}", variables=variables)
        self.refused_before_the_build(result, "secret slot alpha: source not declared")

    def test_a_missing_input_is_refused_before_the_build_without_fallback(self):
        reading, writing = os.pipe()
        self.addCleanup(lambda: [os.close(fd) for fd in (reading, writing) if fd_open(fd)])
        os.write(writing, ALPHA)
        os.close(writing)
        # `from` is unset; the declared `also_from` descriptor holds a value and is not used.
        self.declare(alpha_also=[f"fd:{reading}"])
        self.refused_before_the_build(self.grimoire("run", "app", pass_fds=(reading,), variables={"GRIMOIRE_TEST_BETA": BETA.decode()}),
                                      "secret slot alpha: missing input (env:GRIMOIRE_TEST_ALPHA)")
        # A bound descriptor that was never open, and one whose writer never closes.
        self.refused_before_the_build(self.grimoire("run", "app", "--bind", f"alpha=fd:{reading}", variables={"GRIMOIRE_TEST_BETA": BETA.decode()}),
                                      f"secret slot alpha: unreadable descriptor (fd:{reading})")
        held_reading, held_writing = os.pipe()
        self.addCleanup(lambda: [os.close(fd) for fd in (held_reading, held_writing) if fd_open(fd)])
        os.write(held_writing, ALPHA)
        self.declare(alpha_also=[f"fd:{held_reading}"])
        import contextlib
        import io
        from types import SimpleNamespace
        from unittest.mock import patch
        from registry_run import load_cli
        cli = load_cli()
        real_select = cli.select.select
        remaining = []
        def poll(readers, writers, errors, timeout):
            remaining.append(timeout)
            return real_select(readers, writers, errors, 0)
        # First poll reads the real partial bytes. Four logical seconds later the
        # kernel still sees the writer open with no more bytes. The same deadline
        # must give the second poll only one second, never a new five seconds.
        clock = iter([100.0, 100.0, 104.0])
        out, err = io.StringIO(), io.StringIO()
        with patch.object(cli, "time", SimpleNamespace(monotonic=lambda: next(clock))), \
             patch.object(cli, "select", SimpleNamespace(select=poll)), \
             patch.object(cli, "INHERITED_DESCRIPTORS", {held_reading}), \
             patch.object(cli.subprocess, "run", side_effect=AssertionError("tool started before refusal")), \
             contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = cli.cli_main(["run", str(self.app), "--bind", f"alpha=fd:{held_reading}"],
                                {"GRIMOIRE_TEST_BETA": BETA.decode()})
        self.assertEqual(remaining, [5.0, 1.0])
        self.refused_before_the_build(subprocess.CompletedProcess([], code, out.getvalue(), err.getvalue()),
                                      f"secret slot alpha: descriptor not closed in time (fd:{held_reading})")

    def test_a_registry_dependency_cannot_supply_or_widen_a_secret_grant(self):
        """Grants come only from the executable's own manifest and document: a dependency's
        manifest and grant document declaring a secret, or another source for one, count for
        nothing."""
        package = self.root / "publisher/config-reader"
        shutil.copytree(ROOT / "examples/local-run/config-reader", package)
        reading, writing = os.pipe()
        os.write(writing, ALPHA)
        os.close(writing)
        # Closed at the end only if it is still this pipe: in process, the number of one closed
        # meanwhile may be another file's by then.
        pipe = os.fstat(reading)
        same = lambda status: (status.st_dev, status.st_ino) == (pipe.st_dev, pipe.st_ino)
        self.addCleanup(lambda: fd_open(reading) and same(os.fstat(reading)) and os.close(reading))
        with (package / "witchy.toml").open("a") as stream:
            stream.write('\n[grants]\ndocument = "reader.grants.toml"\n')
        (package / "reader.grants.toml").write_text(
            f'[secrets]\nalpha = {{ use = "seal", from = "env:GRIMOIRE_TEST_ALPHA", also_from = ["fd:{reading}"] }}\n'
            'beta = { use = "seal", from = "env:GRIMOIRE_TEST_BETA" }\n')
        # The payload that crosses the publication boundary has no manifest or grants.
        # Stage the same source under its real dependency name for the two launcher checks.
        from registry_run import coven_pack
        record, separator, source = coven_pack(package).partition(b"\n\n")
        self.assertEqual((record, separator, source),
                         (b"witchy-package 1\nname config-reader\nversion 0.1.0", b"\n\n",
                          (package / "reader.wy").read_bytes()))
        (self.app / "main.packages").write_text("config-reader 0.1.0: ../publisher/config-reader/reader.wy\n")
        (self.app / "main.wy").write_text('import "@config-reader"\n' + PROGRAM)
        (self.app / "main.policy").write_text("<standard>/: *\nmain.wy: *\n@config-reader: File<Read> ; no unchecked\n")
        self.seen.unlink(missing_ok=True)
        variables = {"GRIMOIRE_TEST_ALPHA": ALPHA.decode(), "GRIMOIRE_TEST_BETA": BETA.decode()}
        # Widen: only the dependency lists the descriptor as alpha's source.
        self.refused_before_the_build(self.grimoire("run", "app", "--bind", f"alpha=fd:{reading}", pass_fds=(reading,), variables=variables),
                                      f"secret slot alpha: source not declared (neither from nor also_from) (fd:{reading})")
        # Supply: the executable's document declares no secrets; the dependency's does.
        (self.app / "main.grants.toml").write_text("[secrets]\n")
        result = self.grimoire("run", "app", variables=variables)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("the document grants none ([secrets].", result.stderr)
        # The executable's module never started.
        self.assertFalse([r for r in self.records() if r["tool"] == "WITCHY_WASMTIME_HOST" and r["argv"][1].startswith("/dev/fd/")])
        self.assert_no_canary(result)

    def test_a_registry_dependency_cannot_supply_or_widen_a_secret_grant_through_the_launcher(self):
        self.test_a_registry_dependency_cannot_supply_or_widen_a_secret_grant()


def module_descriptor(engine):
    """The inherited descriptor holding the removed staged module."""
    module = engine["argv"][1]
    prefix = "--module-fd="
    assert module.startswith(prefix) and module[len(prefix):].isdigit(), module
    return int(module[len(prefix):])


def lifeline_descriptor(engine):
    [value] = [arg.removeprefix("--lifeline=fd:") for arg in engine["argv"] if arg.startswith("--lifeline=fd:")]
    return int(value)


class StoreRun(unittest.TestCase):
    """A `SecretStore` through `grimoire run`: each of its secrets is the launch variable of its
    name, read by Grimoire before anything runs and handed to the engine on a pipe of its own
    (`--secret-seal=NAME@fd:N`), as a fixed secret's is. Same wrappers, same canaries."""

    # The same temporary project, wrappers and checks as SecretRun's, without its tests.
    declare, grimoire, records = SecretRun.declare, SecretRun.grimoire, SecretRun.records
    assert_no_canary, refused_before_the_build = SecretRun.assert_no_canary, SecretRun.refused_before_the_build

    def setUp(self):
        SecretRun.setUp(self)
        (self.app / "main.wy").write_text(STORE_PROGRAM)
        (self.app / "main.grants.toml").write_text(STORE_DOCUMENT)
        self.variables = {"GRIMOIRE_TEST_SIGNING": SIGNING.decode(), "GRIMOIRE_TEST_TOKEN": TOKEN.decode()}

    def expected(self):
        return f"{tag(SIGNING, ALPHA_MESSAGE)}\nsealed only\n{len(TOKEN)}\n{tag(TOKEN, BETA_MESSAGE)}\nnot granted\n"

    def assert_engine_got_only_its_store_pipes(self, names, slots=()):
        engines = [r for r in self.records() if r["tool"] == "WITCHY_WASMTIME_HOST"]
        self.assertEqual(len(engines), 1)
        engine = engines[0]
        self.assertLessEqual(set(engine["env"]), {"LC_CTYPE", "__CF_USER_TEXT_ENCODING"})
        stored = [a for a in engine["argv"] if a.startswith(("--secret=", "--secret-seal="))]
        self.assertEqual(sorted(f.partition("@")[0] for f in stored), sorted(names))
        self.assertTrue(all("@fd:" in f for f in stored), stored)
        slotted = [a for a in engine["argv"] if a.startswith("--secret-slot=")]
        self.assertEqual(sorted(f.split(":")[0] for f in slotted), sorted(f"--secret-slot={s}" for s in slots))
        pipes = sorted([int(f.rsplit("@fd:", 1)[1]) for f in stored] + [int(f.rsplit("fd:", 1)[1]) for f in slotted])
        self.assertEqual(engine["descriptors"], sorted(pipes + [module_descriptor(engine), lifeline_descriptor(engine)]))
        self.assertFalse([a for a in engine["argv"] if a.startswith(("--env", "--secret-object="))])

    def test_a_fixed_slot_and_store_may_share_a_name_with_distinct_sources(self):
        from registry_run import load_cli
        cli = load_cli()
        document = self.app / "main.grants.toml"
        document.write_text('[secrets]\nstore = { token = "seal" }\ntoken = { use = "seal", from = "env:OTHER" }\n')
        fixed = cli.read_secret_slots(document)
        store = cli.read_secret_store(document, fixed)
        launch = {"OTHER": ALPHA, "token": BETA}
        self.assertEqual(cli.resolve_secrets(fixed, cli.select_secret_sources(fixed, []), launch), {"token": ALPHA})
        self.assertEqual(cli.resolve_store(store, launch), {"token": BETA})

    def test_a_store_beside_a_fixed_secret_of_the_same_name(self):
        """A slot and a store's secret are two routes with two sources, each on its own pipe,
        even when the slot is named like the store's secret."""
        (self.app / "main.wy").write_text(STORE_PROGRAM.replace(
            "fn main(console: Console<Write>, allocator: Allocator, store: SecretStore):\n",
            "fn main(console: Console<Write>, allocator: Allocator, store: SecretStore, token: Secret<Seal>):\n"
            f'    endorse(&console, &allocator, &token, "{ALPHA_MESSAGE}")\n'
            '    match store.get_sealed("token"):\n        Ok(key):\n'
            f'            endorse(&console, &allocator, &key, "{ALPHA_MESSAGE}")\n'
            '        Err(e):\n            console.line("no key")\n'))
        (self.app / "main.grants.toml").write_text(
            '[secrets]\nstore = { GRIMOIRE_TEST_SIGNING = "seal", GRIMOIRE_TEST_TOKEN = "reveal", token = "seal" }\n'
            'token = { use = "seal", from = "env:GRIMOIRE_TEST_ALPHA" }\n')
        result = self.grimoire("run", "app", variables={**self.variables, "GRIMOIRE_TEST_ALPHA": ALPHA.decode(), "token": BETA.decode()})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"{tag(ALPHA, ALPHA_MESSAGE)}\n{tag(BETA, ALPHA_MESSAGE)}\n" + self.expected())
        self.assertEqual({r["tool"] for r in self.records()}, {"WITCHYC", "WITCHY_WASMTIME_HOST"})
        self.assertTrue(all(r["descriptors"] == [] for r in self.records() if r["tool"] != "WITCHY_WASMTIME_HOST"))
        self.assert_no_canary(result)
        self.assert_engine_got_only_its_store_pipes(["--secret-seal=GRIMOIRE_TEST_SIGNING", "--secret=GRIMOIRE_TEST_TOKEN", "--secret-seal=token"], ["token"])

    def test_a_missing_store_variable_is_refused_before_the_build(self):
        self.refused_before_the_build(self.grimoire("run", "app", variables={"GRIMOIRE_TEST_SIGNING": SIGNING.decode()}),
                                      "secret GRIMOIRE_TEST_TOKEN: missing input (env:GRIMOIRE_TEST_TOKEN)")

    def test_a_store_secret_on_a_second_route_is_refused_before_the_build(self):
        """One source, one route: a store's secret's variable is no slot's source (from or
        also_from), no ordinary environment through [env], and no toolchain variable."""
        cases = [
            ('alpha = { use = "seal", from = "env:GRIMOIRE_TEST_BETA", also_from = ["env:GRIMOIRE_TEST_SIGNING"] }\n',
             "secret GRIMOIRE_TEST_SIGNING: source env:GRIMOIRE_TEST_SIGNING is also slot alpha's source"),
            ('alpha = { use = "seal", from = "env:GRIMOIRE_TEST_TOKEN" }\n',
             "secret GRIMOIRE_TEST_TOKEN: source env:GRIMOIRE_TEST_TOKEN is also slot alpha's source"),
            ('[env]\nenvironment = ["GRIMOIRE_TEST_TOKEN"]\n',
             "secret GRIMOIRE_TEST_TOKEN: source env:GRIMOIRE_TEST_TOKEN is also granted through [env]"),
        ]
        for extra, message in cases:
            with self.subTest(message):
                (self.app / "main.grants.toml").write_text(STORE_DOCUMENT + extra)
                self.refused_before_the_build(self.grimoire("run", "app", variables={**self.variables, "GRIMOIRE_TEST_BETA": BETA.decode()}), message)
        for name, message in [("LANG", "is a variable the toolchain is given"), ("WITCHY_PROOF_JOBS", "is a variable the toolchain is given"),
                              ("SSL_CERT_FILE", "is a variable the toolchain is given"),
                              ("WITCHY_STORE_KEY", "is a WITCHY_ variable, reserved for the toolchain")]:
            with self.subTest(name):
                (self.app / "main.grants.toml").write_text(f'[secrets]\nstore = {{ {name} = "seal" }}\n')
                self.refused_before_the_build(self.grimoire("run", "app", variables={**self.variables, name: SIGNING.decode()}),
                                              f"secret {name}: source env:{name} {message}")

    def test_a_store_entry_that_is_not_a_use_is_refused_before_the_build(self):
        """Refused with the document's error, not a traceback (review of 57fc8fd)."""
        for value in ['["seal"]', '{ a = 1 }', '"sign"', '1']:
            with self.subTest(value):
                (self.app / "main.grants.toml").write_text(f"[secrets]\nstore = {{ GRIMOIRE_TEST_SIGNING = {value} }}\n")
                result = self.grimoire("run", "app", variables=self.variables)
                self.refused_before_the_build(result, "[secrets].store.GRIMOIRE_TEST_SIGNING is not a store's secret Grimoire can launch")
                self.assertNotIn("Traceback", result.stderr)

    def test_a_file_grant_alias_is_refused_with_only_a_store(self):
        """The `[files]` gate holds whenever the program takes secrets, a store's included: a
        descriptor alias or a missing file is refused before the engine starts."""
        (self.app / "main.wy").write_text(STORE_PROGRAM.replace("import secret\n", "import secret\nimport fs\n").replace(
            "store: SecretStore):\n", "store: SecretStore, config: File<Read>):\n"
            "    match config.read_text(&allocator):\n        Ok(text):\n            console.write_string(&text)\n"
            "            console.line(\"\")\n            text.close()\n        Err(e):\n            console.line(\"unreadable\")\n"))
        for name, message in [("missing.txt", "does not exist"), ("/dev//fd/3", "is a descriptor alias")]:
            with self.subTest(name):
                (self.app / "main.grants.toml").write_text(STORE_DOCUMENT + f'\n[files]\nconfig = "{name}"\n')
                result = self.grimoire("run", "app", variables=self.variables)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertFalse([r for r in self.records() if r["tool"] == "WITCHY_WASMTIME_HOST"], "the engine never started")
                self.assert_no_canary(result)

    def test_a_store_no_parameter_takes_is_refused(self):
        """An over-granted store (no `SecretStore` parameter): its values were read for nothing,
        and nothing runs. Never handed to the engine."""
        (self.app / "main.wy").write_text(PROGRAM)
        (self.app / "main.grants.toml").write_text(STORE_DOCUMENT + 'alpha = { use = "seal", from = "env:GRIMOIRE_TEST_ALPHA" }\n'
                                                   'beta = { use = "seal", from = "env:GRIMOIRE_TEST_BETA" }\n')
        result = self.grimoire("run", "app", variables={**self.variables, "GRIMOIRE_TEST_ALPHA": ALPHA.decode(), "GRIMOIRE_TEST_BETA": BETA.decode()})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("secret GRIMOIRE_TEST_SIGNING: declared, and wasm-grants proposes no store's secret for it", result.stderr)
        self.assertFalse([r for r in self.records() if r["tool"] == "WITCHY_WASMTIME_HOST"], "the engine never started")
        self.assert_no_canary(result)

    def test_a_file_grant_alias_is_refused_with_only_a_store_through_the_launcher(self):
        self.test_a_file_grant_alias_is_refused_with_only_a_store()

    def test_a_store_no_parameter_takes_is_refused_through_the_launcher(self):
        self.test_a_store_no_parameter_takes_is_refused()


def fd_open(number):
    try:
        os.fstat(number)
        return True
    except OSError:
        return False


if __name__ == "__main__":
    unittest.main()
