"""Integration checks for registry dependencies, using the configured witchyc,
witchy-wasmtime-host and coven (the COVEN launcher)."""

import contextlib
import functools
import hashlib
import sys
import json
import os
import shlex
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
OPERATOR_ID = "witchy-2:73615fc5aaa407846b296677b2803e29c3f555db426c69103a3cb45d584730c531ba2d818786b2a9d09b13100e3174f1"
LOCKED_PROJECT = os.environ.pop("GRIMOIRE_TEST_LOCKED_PROJECT", None)
DEPENDENCY = '\n[registry]\npath = "../../registry"\n\n[dependencies]\nconfig-reader = "0.1.0"\n'


def locked_project():
    """The suite's shared locked project (`make_locked_project`, made by run.py while the first
    cases run), once it is complete; None when the suite made none."""
    if not LOCKED_PROJECT:
        return None
    import fcntl
    source = Path(LOCKED_PROJECT)
    with open(source / ".building", "rb") as building:
        fcntl.flock(building, fcntl.LOCK_SH)   # held exclusively until the project is complete
    if not (source / "complete").is_file():
        raise RuntimeError(f"{source}: the suite's locked project was not made (the runner says why)")
    return source


def pin_of(path):
    """A file's pin as the compiler's policies and Grimoire's records write it (the witchy-2 profile)."""
    return "sha384:" + hashlib.sha384(Path(path).read_bytes()).hexdigest()


def version_digest(slot):
    """What a version's digest is of: its `package` record, then its source (Coven)."""
    return "sha384:" + hashlib.sha384((slot / "package").read_bytes() + (slot / "source.wy").read_bytes()).hexdigest()


@functools.cache
def inline_tools():
    # The two standalone repos can also be tested with a released Coven.
    # In that configuration use the same subprocess path as production.
    executable = shutil.which(os.environ.get("COVEN", "coven"))
    if executable is None:
        return None
    path = Path(executable).resolve()
    if not (path.parent / "cli_test_support.py").is_file():
        return None
    sys.path.insert(0, str(path.parent))
    try:
        import cli_test_support
    finally:
        sys.path.pop(0)
    return cli_test_support


def coven_pack(package):
    """What `coven pack PACKAGE` writes: Coven's own command, run in this process when the suite
    has Coven's test support (as the registry commands here are), else as a process."""
    coven = os.environ.get("COVEN", "coven")
    support = inline_tools()
    if support is None:
        return subprocess.run([coven, "pack", package], capture_output=True, check=True).stdout
    import io
    out, err = io.TextIOWrapper(io.BytesIO(), encoding="utf-8"), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        status = support.fresh_coven(shutil.which(coven)).main(["pack", str(package)])
    out.flush()
    if status:
        raise subprocess.CalledProcessError(status, [coven, "pack", package], out.buffer.getvalue(), err.getvalue())
    return out.buffer.getvalue()


@functools.cache
def cli_code():
    import importlib.machinery
    loader = importlib.machinery.SourceFileLoader("grimoire_cli", str(ROOT / "grimoire"))
    return loader, loader.get_code(loader.name)


def load_cli():
    import importlib.util
    loader, code = cli_code()
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    exec(code, module.__dict__)
    return module


@functools.cache
def capture_files():
    """The two files `capture_children` collects into, made once per test process (each call
    empties them first)."""
    return tempfile.TemporaryFile(mode="w+"), tempfile.TemporaryFile(mode="w+")


@contextlib.contextmanager
def capture_children():
    """Collect output from both Python and the real compiler/host, including inherited stdio."""
    out, err = capture_files()
    for stream in (out, err):
        stream.seek(0)
        stream.truncate()
    stdout, stderr = os.dup(1), os.dup(2)
    try:
        sys.stdout.flush()
        sys.stderr.flush()
        os.dup2(out.fileno(), 1)
        os.dup2(err.fileno(), 2)
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            yield out, err
    finally:
        os.dup2(stdout, 1)
        os.dup2(stderr, 2)
        os.close(stdout)
        os.close(stderr)


def served(path, data):
    """`path` holds `data`, in a folder made if missing; written only if it holds anything else."""
    try:
        if path.read_bytes() == data:
            return
    except FileNotFoundError:
        path.parent.mkdir(exist_ok=True)
    path.write_bytes(data)


@functools.cache
def umask():
    mask = os.umask(0o022)
    os.umask(mask)
    return mask


@functools.cache
def example_files(name):
    """examples/local-run/NAME's files (it has no folders): (name, mode, bytes) of each."""
    folder = ROOT / "examples/local-run" / name
    entries = sorted(folder.iterdir())
    if not all(entry.is_file() and not entry.is_symlink() for entry in entries):
        raise ValueError(f"{folder}: only files are copied")
    return tuple((entry.name, entry.stat().st_mode & 0o7777, entry.read_bytes()) for entry in entries)


def example(name, destination, changes={}):
    """A copy of examples/local-run/NAME at `destination`, as `shutil.copytree` makes it apart
    from times and flags; `changes` maps a file to None (left out) or to a function of its bytes."""
    destination.mkdir(parents=True)
    for file, mode, data in example_files(name):
        change = changes.get(file, lambda data: data)
        if file in changes and change is None:
            continue
        (destination / file).write_bytes(change(data))
        if mode != 0o666 & ~umask():
            (destination / file).chmod(mode)


class RegistryRun(unittest.TestCase):
    def use_policy_signatures(self, *, packages=False):
        support = inline_tools()
        if hasattr(support, "policy_signatures"):
            self.enterContext(support.policy_signatures(packages=packages))

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        (self.root / "registry").mkdir()
        # Coven keeps an owner's copy of each index it signs under XDG_STATE_HOME: per test.
        previous = os.environ.get("XDG_STATE_HOME")
        os.environ["XDG_STATE_HOME"] = str(self.root / "state")
        self.addCleanup(lambda: os.environ.pop("XDG_STATE_HOME") if previous is None else os.environ.update(XDG_STATE_HOME=previous))
        # The two examples, the consumer without its local package list and with the registry
        # dependency (`example`: written from one reading of each).
        example("config-reader", self.root / "publisher/config-reader")
        example("app", self.root / "consumer/app", {"main.packages": None, "witchy.toml": lambda text: text + DEPENDENCY.encode()})
        self.app = self.root / "consumer/app"
        self.slot = self.root / "registry/config-reader/0.1.0"
        # The registry's operator: the project declares its key, and every grimoire call below is
        # made after it signs a snapshot (`stamp`).
        # NIST's ACVP ML-DSA-65 key-generation seed (synthetic), its public key from `coven key`:
        # fixed, so that no test spends an engine run deriving it.
        self.operator = self.root / "operator"
        self.operator.write_bytes((ROOT / "tests/fixtures/operator/seed").read_bytes())
        self.operator.chmod(0o600)
        Path(f"{self.operator}.pub").write_bytes((ROOT / "tests/fixtures/operator/seed.pub").read_bytes())
        self.operator_print = OPERATOR_ID
        manifest = self.app / "witchy.toml"
        manifest.write_text(manifest.read_text().replace('[registry]\n', f'[registry]\noperator = "{self.operator_print}"\nid = "test"\n', 1))

    def stamp(self, registry="registry"):
        """A snapshot of whatever the registry serves now, signed with the operator's key: an
        operator that signs anything (compromised, or fooled), so that each package's own checks,
        which these tests exercise, are what must refuse a writer's changes. `coven snapshot`, the
        operator's tool, admits only what each owner moved to (`snapshot`, used by the snapshot tests)."""
        import datetime
        coven = self.load_coven()
        root = self.root / registry
        entries = {}
        for package in sorted(root.iterdir()):
            if not package.is_dir() or package.is_symlink() or not coven.NAME.fullmatch(package.name) \
                    or not any(coven.VERSION.fullmatch(part.name) for part in package.iterdir()):
                continue
            index = package / "index"
            if not index.is_file():
                entries[package.name] = None
                continue
            raw = index.read_bytes()
            text = raw.decode("ascii", "replace")
            sequence = re.search(r"^sequence ([1-9][0-9]*)$", text, re.M)
            recovery = re.search(r"^recovery (none|witchy-2:[0-9a-f]{96})$", text, re.M)
            signer = re.search(r"^signer (witchy-2 [A-Za-z0-9+/=]+)$", text, re.M)
            try:
                owner = coven.key_id(coven.read_public_key(signer.group(1), "signer")) if signer else None
            except coven.CovenError:
                owner = None
            sequence = int(sequence.group(1)) if sequence else 1
            served(package / "indexes" / str(sequence), raw)
            # The rotations served now, bound as Coven's operator binds them (`NAME/chains/HEX`).
            rotations = (package / "rotations").read_bytes() if (package / "rotations").is_file() else None
            chain = "none" if rotations is None else "sha384:" + hashlib.sha384(rotations).hexdigest()
            if rotations is not None:
                served(package / "chains" / chain[7:], rotations)
            entries[package.name] = (sequence, "sha384:" + hashlib.sha384(raw).hexdigest(), owner or "witchy-2:" + "a" * 96,
                                     recovery.group(1) if recovery else "none", chain)
        current = root / "snapshot"
        # Signed once per state: the engine signs again only when what it would list, or the
        # snapshot the registry serves, changed since the last stamp.
        if getattr(self, "last_stamp", None) == (registry, entries) and current.is_file() and current.read_bytes() == self.last_stamped:
            return
        previous = re.search(r"^sequence ([0-9]+)$", current.read_bytes().decode("latin-1"), re.M) if current.is_file() else None
        # Above the registry's and above the last this operator signed (its own copy, as Coven's).
        sequence = max(int(previous.group(1)) if previous else 0, getattr(self, "stamped", 0)) + 1
        self.stamped = sequence
        expires = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0) + datetime.timedelta(hours=1)
        self.stamps = getattr(self, "stamps", 0) + 1
        current.write_bytes(self.forge_operator("snapshot", self.operator, lambda public: coven.snapshot_message("test", sequence, expires, entries, public)))
        self.last_stamp, self.last_stamped = (registry, entries), current.read_bytes()

    def coven(self, *args, env=None):
        command = [os.environ.get("COVEN", "coven"), *args]
        support = inline_tools()
        if support:
            return support.invoke_coven(command, self.root, env)
        return subprocess.run(command, cwd=self.root, capture_output=True, text=True, timeout=60, env=env)

    def grimoire(self, *args, env=None, first_use=True, stamp=True):
        # These tests exercise first use on purpose: `lock` trusts it explicitly unless a test asks
        # for the default (`first_use=False`), which refuses it.
        if args and args[0] == "lock" and first_use:
            args = ("lock", "--trust-first-use", *args[1:])
        if stamp and (self.root / "registry").is_dir():
            self.stamp()
        support = inline_tools()
        # The worker owns its cwd and stdio. Execute the full CLI for environment,
        # descriptor and custom-toolchain checks, and the signed E2E flow.
        state_only = env is None or env == dict(os.environ, XDG_STATE_HOME=env.get("XDG_STATE_HOME", ""))
        if support and (state_only or env.get("COVEN") == os.environ.get("COVEN")):
            sys.audit("unittest.cli_inline", "grimoire", args[0])
            module = support.fresh(ROOT / "grimoire")
            module.INHERITED_DESCRIPTORS = frozenset()
            with contextlib.chdir(self.root), support.command_environment(env), capture_children() as (out, err):
                module.run_coven = lambda cmd: support.invoke_coven(cmd, self.root, module.toolchain_environment())
                status = module.cli_main(list(map(str, args)), {})
                out.seek(0)
                err.seek(0)
                return subprocess.CompletedProcess(args, status, out.read(), err.read())
        return subprocess.run([ROOT / "grimoire", *args], cwd=self.root, capture_output=True,
                              text=True, timeout=60, env=env)

    def publish(self):
        published = self.coven("publish", "publisher/config-reader", "--registry", "registry")
        self.assertEqual(published.returncode, 0, published.stderr)
        return published

    def locked(self):
        source = locked_project()
        if source and not any((self.root / "registry").iterdir()):
            shutil.copytree(source / "registry", self.root / "registry", dirs_exist_ok=True)
            shutil.copy2(source / "witchy.lock", self.app / "witchy.lock")
            # The trust store filename includes the registry's absolute path.
            # Preserve the actual on-disk evidence from the original lock.
            cli = load_cli()
            store = cli.TrustStore(self.root / "registry")
            try:
                shutil.copy2(source / "trust.json", store.path)
            finally:
                store.close()
            self.stamped, entries = json.loads((source / "stamp.json").read_text())
            self.last_stamp = ("registry", entries)
            self.last_stamped = (self.root / "registry/snapshot").read_bytes()
            return
        self.publish()
        result = self.grimoire("lock", "consumer/app")
        self.assertEqual(result.returncode, 0, result.stderr)

    def refused(self, result, message):
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn(message, result.stderr)


    def test_registry_dependencies_need_the_executables_policy(self):
        self.use_policy_signatures(packages=True)
        self.locked()
        (self.app / "main.policy").unlink()
        self.refused(self.grimoire("run", "consumer/app"), "main.policy is missing")

    def test_the_policy_holds_a_package_alike_from_a_checkout_and_the_registry(self):
        """main holds File<Read>; a policy that gives config-reader nothing refuses it in both
        modes, with the same words, before the host is started."""
        self.use_policy_signatures(packages=True)
        self.locked()
        log = self.root / "host.log"
        host = self.root / "logged-host"
        real = shutil.which(os.environ.get("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))
        host.write_text(f'#!/bin/sh\necho "$1" >> "{log}"\nexec "{real}" "$@"\n')
        host.chmod(0o755)
        env = dict(os.environ, WITCHY_WASMTIME_HOST=str(host), COVEN=os.environ["COVEN"])
        checkout = self.root / "checkout"
        shutil.copytree(ROOT / "examples/local-run", checkout)
        denied = "@config-reader can use File<Read>, which the policy does not allow"
        for app in [self.app, checkout / "app"]:
            policy = app / "main.policy"
            policy.write_text(policy.read_text().replace("@config-reader: File<Read>", "@config-reader: nothing"))
            self.refused(self.grimoire("run", app, env=env), denied)
        # Coven's own fetch program runs; the app's module never does.
        ran = log.read_text().split() if log.exists() else []
        self.assertFalse([line for line in ran if line.startswith("/dev/fd/")], ran)

    def test_an_update_runs_only_after_the_author_reviews_it(self):
        """A new version locks, and the policy's reviewed digest stops it until it is repinned."""
        self.use_policy_signatures(packages=True)
        self.locked()
        reader = self.root / "publisher/config-reader"
        manifest = reader / "witchy.toml"
        manifest.write_text(manifest.read_text().replace('"0.1.0"', '"0.1.1"'))
        with (reader / "reader.wy").open("a") as stream:
            stream.write("\npub fn default_port() -> U64:\n    return 3000\n")
        self.publish()
        app_manifest = self.app / "witchy.toml"
        app_manifest.write_text(app_manifest.read_text().replace('"0.1.0"', '"0.1.1"'))
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        self.refused(self.grimoire("run", "consumer/app"), "@config-reader has changed since its line was reviewed")
        reviewed = self.grimoire("review", "--accept", "consumer/app")
        self.assertEqual(reviewed.returncode, 0, reviewed.stderr)
        self.assertIn("@config-reader: 1 reachable functions; can use File<Read>", reviewed.stdout)
        # The policy pins the reviewed source; the lock pins the version Coven published.
        reviewed = "sha384:" + hashlib.sha384((reader / "reader.wy").read_bytes()).hexdigest()
        self.assertIn(reviewed, (self.app / "main.policy").read_text())
        self.assertIn((self.slot.parent / "0.1.1/digest").read_text(), (self.app / "witchy.lock").read_text())
        result = self.grimoire("run", "consumer/app")
        self.assertEqual((result.returncode, result.stdout), (0, "8080\n"), result.stderr)

    def test_accepting_a_review_widens_nothing(self):
        self.use_policy_signatures(packages=True)
        self.locked()
        policy = self.app / "main.policy"
        policy.write_text(policy.read_text().replace("@config-reader: File<Read>", "@config-reader: nothing"))
        before = policy.read_text()
        reviewed = self.grimoire("review", "--accept", "consumer/app")
        self.assertEqual(reviewed.returncode, 1)
        self.assertIn("@config-reader can use File<Read>, which the policy does not allow", reviewed.stdout + reviewed.stderr)
        self.assertEqual(policy.read_text(), before)

    def test_review_drafts_a_missing_policy_and_writes_none(self):
        self.use_policy_signatures(packages=True)
        self.locked()
        (self.app / "main.policy").unlink()
        reviewed = self.grimoire("review", "consumer/app")
        self.assertEqual(reviewed.returncode, 1)
        self.assertIn("@config-reader: File<Read> ; no unchecked ; digest " + pin_of(ROOT / "examples/local-run/config-reader/reader.wy"), reviewed.stdout)
        self.assertFalse((self.app / "main.policy").exists())

    def publish_package(self, name, version, source, requires=None):
        folder = self.root / "publisher" / f"{name}-{version}"
        folder.mkdir(parents=True)
        (folder / f"{name}.wy").write_text(source)
        listed = "".join(f'{n} = "{v}"\n' for n, v in (requires or {}).items())
        (folder / "witchy.toml").write_text(f'[package]\nname = "{name}"\nversion = "{version}"\nsource = "{name}.wy"\n'
                                            f"\n[dependencies]\n{listed}")
        published = self.coven("publish", folder, "--registry", "registry")
        self.assertEqual(published.returncode, 0, published.stderr)

    def transitive(self, digits_version="1.0.0", reader_version="0.2.0", body="byte - 48"):
        """config-reader READER_VERSION, which reads digits through digits DIGITS_VERSION."""
        self.publish_package("digits", digits_version,
                             f"pub fn is_digit(byte: U64) -> Bool:\n    return byte >= 48 && byte <= 57\n\n"
                             f"pub fn digit_value(byte: U64) -> U64:\n    return {body}\n")
        reader = (ROOT / "examples/local-run/config-reader/reader.wy").read_text()
        reader = 'import "@digits"\n\n' + reader.replace("byte >= 48 && byte <= 57", "is_digit(byte)").replace("(byte - 48)", "digit_value(byte)")
        self.publish_package("config-reader", reader_version, reader, {"digits": digits_version})
        manifest = self.app / "witchy.toml"
        manifest.write_text(re.sub(r'config-reader = "[0-9.]+"', f'config-reader = "{reader_version}"', manifest.read_text()))

    def review_policy(self):
        policy = self.app / "main.policy"
        # Digests to be set by the review: `--accept` pins the lines that carry one.
        unset = "sha384:" + "0" * 96
        policy.write_text(f"<standard>/: *\nmain.wy: *\n@config-reader: File<Read> ; no unchecked ; digest {unset}\n"
                          f"@digits: nothing ; no unchecked ; digest {unset}\n")
        reviewed = self.grimoire("review", "--accept", "consumer/app")
        self.assertEqual(reviewed.returncode, 0, reviewed.stdout + reviewed.stderr)

    def test_the_lock_holds_the_whole_graph(self):
        self.use_policy_signatures(packages=True)
        self.transitive()
        locked = self.grimoire("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertIn("locked digits 1.0.0", locked.stdout)
        self.assertIn("(required by config-reader 0.2.0)", locked.stdout)
        lock = (self.app / "witchy.lock").read_text()
        self.assertIn('requires = ["digits 1.0.0"]', lock)
        self.assertIn((self.root / "registry/digits/1.0.0/digest").read_text(), lock)
        self.review_policy()
        result = self.grimoire("run", "consumer/app")
        self.assertEqual((result.returncode, result.stdout), (0, "8080\n"), result.stderr)
        # A lock missing a package of the graph is not run.
        trimmed = "\n[[package]]".join(block for block in lock.split("\n[[package]]") if 'name = "digits"' not in block)
        (self.app / "witchy.lock").write_text(trimmed)
        self.refused(self.grimoire("run", "consumer/app"), "does not pin digits 1.0.0 (required by config-reader 0.2.0)")


    def test_a_transitive_update_runs_only_after_review(self):
        """The report's case: a correctly published release deep in the graph changes code."""
        self.use_policy_signatures(packages=True)
        self.transitive()
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        self.review_policy()
        self.transitive("1.0.1", "0.2.1", body="(byte - 48) % 10")
        locked = self.grimoire("lock", "consumer/app")
        self.assertIn("locked digits 1.0.1", locked.stdout, locked.stderr)
        self.refused(self.grimoire("run", "consumer/app"), "@digits has changed since its line was reviewed")
        reviewed = self.grimoire("review", "consumer/app")
        self.assertIn("@digits: 2 reachable functions", reviewed.stdout)
        self.assertEqual(self.grimoire("review", "--accept", "consumer/app").returncode, 0)
        result = self.grimoire("run", "consumer/app")
        self.assertEqual((result.returncode, result.stdout), (0, "8080\n"), result.stderr)

    def test_a_pinned_toolchain_is_the_one_that_builds(self):
        self.use_policy_signatures(packages=True)
        self.publish()
        pinned = self.grimoire("lock", "--toolchain", "consumer/app")
        self.assertEqual(pinned.returncode, 0, pinned.stderr)
        compiler = shutil.which(os.environ.get("WITCHYC", "witchyc"))
        lock = (self.app / "witchy.lock").read_text()
        self.assertIn('compiler = "sha384:' + hashlib.sha384(Path(compiler).read_bytes()).hexdigest(), lock)
        host = Path(shutil.which(os.environ["WITCHY_WASMTIME_HOST"])).resolve()
        for library in host.parent.glob("libaws_lc_fips_*crypto*"):
            self.assertIn(f'"{library.name}" = "sha384:' + hashlib.sha384(library.read_bytes()).hexdigest(), lock)
        linked = self.root / "linked-host"
        linked.symlink_to(host)
        for library in host.parent.glob("libaws_lc_fips_*crypto*"):
            (self.root / library.name).write_bytes(b"decoy beside the link")
        result = self.grimoire("run", "consumer/app", env=dict(os.environ, WITCHY_WASMTIME_HOST=str(linked)))
        self.assertEqual((result.returncode, result.stdout), (0, "8080\n"), result.stderr)
        # The pin is kept by a plain relock, and another compiler is refused before compiling.
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        self.assertIn("[toolchain]", (self.app / "witchy.lock").read_text())
        other = self.root / "other-witchyc"
        shutil.copyfile(compiler, other)
        with other.open("ab") as stream:
            stream.write(b"\0")
        other.chmod(0o755)
        self.refused(self.grimoire("run", "consumer/app", env=dict(os.environ, WITCHYC=str(other))), "pins the compiler at")
        # Every command that compiles or verifies checks the pin, not only `run` (final-state review).
        for command in [("lock", "consumer/app"), ("review", "--accept", "consumer/app")]:
            self.refused(self.grimoire(*command, env=dict(os.environ, WITCHYC=str(other))), "pins the compiler at")

    def test_a_pinned_toolchain_covers_the_hosts_crypto_library(self):
        """The host loads its FIPS crypto module from its own directory; --toolchain pins it, and a
        swapped library is refused before anything is compiled."""
        # The companion pinned-run test checks actual toolchain bytes on a running app.
        # Here the independent writer pins known bytes, including the real path of
        # a symlinked host. Invoke the refusal through the normal run command.
        folder = self.root / "hostdir"
        folder.mkdir()
        host = folder / "host"
        host.write_bytes(b"host under test")
        host.chmod(0o755)
        compiler = folder / "compiler"
        compiler.write_bytes(b"compiler under test")
        compiler.chmod(0o755)
        library = folder / "libaws_lc_fips_test_crypto.dylib"
        library.write_bytes(b"original library")
        (self.app / "witchy.lock").write_text(
            f'[toolchain]\ncompiler = "{pin_of(compiler)}"\nhost = "{pin_of(host)}"\n'
            f'[toolchain.host-libraries]\n"{library.name}" = "{pin_of(library)}"\n')
        manifest = self.app / "witchy.toml"
        from unittest.mock import patch
        cli = load_cli()
        linked = self.root / "linkdir"
        linked.mkdir()
        (linked / "host").symlink_to(host)
        (linked / library.name).write_bytes(b"decoy")
        for selected in (host, linked / "host"):
            library.write_bytes(b"original library")
            env = dict(os.environ, WITCHY_WASMTIME_HOST=str(selected), WITCHYC=str(compiler))
            with patch.dict(os.environ, env):
                cli.check_toolchain(manifest)
            for missing in (False, True):
                if missing:
                    library.unlink()
                else:
                    library.write_bytes(b"swapped")
                self.refused(self.grimoire("run", "consumer/app", env=env, stamp=False), "pins the host's libraries")

    def test_a_build_record_names_every_input_by_digest(self):
        self.use_policy_signatures(packages=True)
        self.locked()
        record = self.root / "build.json"
        result = self.grimoire("run", "--record", record, "consumer/app")
        self.assertEqual(result.stdout, "8080\n", result.stderr)
        built = json.loads(record.read_text())
        self.assertEqual(built["schema"], "grimoire-build-record 1")
        source = "sha384:" + hashlib.sha384((self.slot / "source.wy").read_bytes()).hexdigest()
        self.assertEqual(built["packages"]["config-reader"], {"source": source, "version": "0.1.0", "digest": version_digest(self.slot), "requires": [], "signer": "unsigned"})
        self.assertEqual(built["sources"], {"main.wy": "sha384:" + hashlib.sha384((self.app / "main.wy").read_bytes()).hexdigest()})
        self.assertEqual(built["policy"], "sha384:" + hashlib.sha384((self.app / "main.policy").read_bytes()).hexdigest())
        self.assertTrue(built["module"].startswith("sha384:") and built["host_flags"])

    def key(self, name):
        """A witchy-2 key: a 32-byte seed only its owner reads, its public key beside it, and its
        key ID. The tests' usual names come from tests/fixtures/keys (synthetic, fixed, so that no
        test spends an engine run deriving them); any other is made fresh with `coven key`."""
        key = self.root / name
        fixture = ROOT / "tests/fixtures/keys" / name
        if fixture.is_file():
            key.write_bytes(fixture.read_bytes())
            key.chmod(0o600)
            Path(f"{key}.pub").write_text(Path(f"{fixture}.pub").read_text())
            return key, self.key_print_of(key)
        key.write_bytes(os.urandom(32))
        key.chmod(0o600)
        made = self.coven("key", "--key", str(key))
        self.assertEqual(made.returncode, 0, made.stderr)
        return key, made.stdout.split()[1]

    def public(self, key):
        """The witchy-2 public key (bytes) of `key`, from its public key file."""
        return self.load_coven().signing_key(key).public

    def publish_reader(self, version, key=None, extra="", registry="registry"):
        folder = self.root / f"reader-{version}"
        example("config-reader", folder, {"witchy.toml": lambda text: text.replace(b'"0.1.0"', f'"{version}"'.encode()),
                                          "reader.wy": lambda text: text + extra.encode()})
        (self.root / registry).mkdir(exist_ok=True)
        published = self.coven("publish", folder, "--registry", registry, *(["--key", str(key)] if key else []))
        self.assertEqual(published.returncode, 0, published.stderr)

    def test_a_package_keeps_its_publisher(self):
        """A new version signed by another key, or unsigned after a signed one, is refused."""
        self.use_policy_signatures(packages=True)
        (alice, alice_print), (mallory, _) = self.key("alice"), self.key("mallory")
        self.publish_reader("0.1.0", alice)
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        self.assertIn(f'signer = "{alice_print}"', (self.app / "witchy.lock").read_text())
        self.assertIn("index = 1", (self.app / "witchy.lock").read_text())
        manifest = self.app / "witchy.toml"
        # Coven refuses to publish these into alice's package, so a registry writer stores them
        # directly; neither is in alice's index.
        for version, key in [("0.1.1", mallory), ("0.1.2", None)]:
            self.publish_reader(version, key, f"// {version}\n", registry=f"elsewhere-{version}")
            shutil.copytree(self.root / f"elsewhere-{version}/config-reader/{version}", self.root / f"registry/config-reader/{version}")
            message = f"refused: config-reader's index, signed by its owner {alice_print}, does not list this version"
            manifest.write_text(re.sub(r'config-reader = "[0-9.]+"', f'config-reader = "{version}"', manifest.read_text()))
            self.refused(self.grimoire("lock", "consumer/app"), message)
        # The same publisher's next version locks, and runs once reviewed.
        self.publish_reader("0.1.3", alice, "// 0.1.3\n")
        manifest.write_text(re.sub(r'config-reader = "[0-9.]+"', 'config-reader = "0.1.3"', manifest.read_text()))
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        self.assertEqual(self.grimoire("review", "--accept", "consumer/app").returncode, 0)
        self.assertEqual(self.grimoire("run", "consumer/app").stdout, "8080\n")
        # The registry's signature removed after locking: the run refuses it.
        (self.root / "registry/config-reader/0.1.3/release-signature").unlink()
        self.refused(self.grimoire("run", "consumer/app"), f"refused: unsigned, and config-reader is owned by {alice_print}")

    def test_a_rotated_package_keeps_running_and_relocks_to_its_new_owner(self):
        self.use_policy_signatures(packages=True)
        (alice, alice_print), (bob, bob_print), (rescue, rescue_print) = self.key("alice"), self.key("bob"), self.key("rescue")
        folder = self.root / "reader-first"
        example("config-reader", folder)
        published = self.coven("publish", folder, "--registry", "registry", "--key", str(alice), "--recovery-key", f"{rescue}.pub")
        self.assertEqual(published.returncode, 0, published.stderr)
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        lock = (self.app / "witchy.lock").read_text()
        self.assertIn(f'signer = "{alice_print}"', lock)
        self.assertIn(f'recovery = "{rescue_print}"', lock)
        self.assertEqual(self.grimoire("review", "--accept", "consumer/app").returncode, 0)
        # The recovery key names the latest index sequence it knows: the consumer's lock has it.
        rotated = self.coven("rotate", "config-reader", "--registry", "registry", "--key", str(rescue), "--new-key", str(bob), "--min-sequence", "1", "--from-owner", alice_print)
        self.assertEqual(rotated.returncode, 0, rotated.stderr)
        # The locked build keeps running, told of the rotation; a relock records the new owner.
        ran = self.grimoire("run", "consumer/app")
        self.assertEqual(ran.stdout, "8080\n", ran.stderr)
        self.assertIn(f"handed by its owner {alice_print} to {bob_print}", ran.stderr)
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        self.assertIn(f'signer = "{bob_print}"', (self.app / "witchy.lock").read_text())
        self.assertIn("index = 2", (self.app / "witchy.lock").read_text())
        # A registry that then drops the rotation statement leaves alice's release outside bob's package: refused.
        (self.root / "registry/config-reader/rotations").unlink()
        self.refused(self.grimoire("run", "consumer/app"), "refused")

    def coven_elsewhere(self, machine, *args):
        """Coven run on another machine: its own owner state, not the consumer's."""
        env = dict(os.environ, XDG_STATE_HOME=str(self.root / f"machine-{machine}"))
        return self.coven(*args, env=env)

    def test_this_machine_keeps_floors_and_pins_when_the_lock_is_deleted(self):
        self.use_policy_signatures(packages=True)
        alice, alice_print = self.key("alice")
        self.assertEqual(self.coven_elsewhere("alice", "publish", "publisher/config-reader", "--registry", "registry", "--key", str(alice)).returncode, 0)
        index = self.root / "registry/config-reader/index"
        before = index.read_text()
        self.assertEqual(self.coven_elsewhere("alice", "refresh", "config-reader", "--registry", "registry", "--key", str(alice)).returncode, 0)
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        # The lock deleted, and the registry serving the older index: still refused here.
        (self.app / "witchy.lock").unlink()
        index.write_text(before)
        self.refused(self.grimoire("lock", "consumer/app"), "older than index 2 already seen")
        # A second project on this machine, with no lock, keeps alice as the package's owner.
        shutil.copytree(self.root / "consumer/app", self.root / "consumer/second")
        self.assertEqual(self.coven_elsewhere("alice", "refresh", "config-reader", "--registry", "registry", "--key", str(alice)).returncode, 0)
        (self.root / "registry/config-reader/0.1.0/release-signature").unlink()
        self.refused(self.grimoire("lock", "consumer/second"), f"and config-reader is owned by {alice_print}")


    def declare(self, text, registry_extra=""):
        manifest = self.app / "witchy.toml"
        body = manifest.read_text()
        if registry_extra:
            body = body.replace('[registry]\n', f'[registry]\n{registry_extra}\n', 1)
        manifest.write_text(body + f"\n[owners]\n{text}\n")


    def forge(self, key, name, message, namespace, out):
        """`message` (a Coven object's bytes, or their text) signed with `key` for the role of
        `namespace`, outside Coven's checks (a writer's or a thief's key), as Coven stores it."""
        role = {"coven-rotation": "rotation", "coven-index": "index", "coven-release": "release"}[namespace]
        coven = self.load_coven()
        data = message if isinstance(message, bytes) else message.encode("latin-1")
        return coven.signed_object(data, coven.witchy_sign(role, coven.signing_key(key), data)).decode("latin-1")


    def operator_key(self, name):
        """Another witchy-2 operator key (a successor, a writer's or a thief's) and its key ID."""
        return self.key(name)

    def forge_operator(self, role, key, message):
        """`message(public key)` signed by the operator key `key` as a witchy-2 object of `role`,
        outside Coven's checks (a stolen or a writer's key), as Coven stores it."""
        coven = self.load_coven()
        held = coven.signing_key(key)
        made = message(held.public)
        return coven.signed_object(made, coven.witchy_sign(role, held, made))

    def load_coven(self):
        support = inline_tools()
        if support:
            return support.fresh_coven(os.environ["COVEN"])
        import importlib.machinery, importlib.util
        loader = importlib.machinery.SourceFileLoader("coven_module", os.environ["COVEN"])
        coven = importlib.util.module_from_spec(importlib.util.spec_from_loader("coven_module", loader))
        loader.exec_module(coven)
        return coven

    def key_print_of(self, key):
        return self.load_coven().signing_key(key).id


    def test_a_first_use_fork_is_refused_until_forgotten(self):
        self.use_policy_signatures(packages=True)
        import datetime
        coven = self.load_coven()
        (alice, alice_print), (bob, _), (mallory, _) = self.key("alice"), self.key("bob"), self.key("mallory")
        self.assertEqual(self.coven_elsewhere("alice", "publish", "publisher/config-reader", "--registry", "registry", "--key", str(alice)).returncode, 0)
        self.assertEqual(self.grimoire("lock", "consumer/app", env=dict(os.environ, XDG_STATE_HOME=str(self.root / "machine-reviewer"))).returncode, 0)
        reviewed = (self.app / "witchy.lock").read_text()
        self.assertEqual(self.coven_elsewhere("alice", "rotate", "config-reader", "--registry", "registry", "--key", str(alice), "--new-key", str(bob), "--no-snapshot").returncode, 0)
        package = self.root / "registry/config-reader"
        honest = (package / "rotations").read_text(), (package / "index").read_text()
        entries = coven.read_index(package, "config-reader").entries
        expires = datetime.date.today() + datetime.timedelta(days=30)
        def fork(sequence, suffix):
            (package / "rotations").write_text(self.forge(alice, "config-reader", coven.rotation_message("config-reader", alice_print, self.public(mallory), self.public(alice)).decode(), "coven-rotation", package / f"rotations-{suffix}"))
            (package / "index").write_text(self.forge(mallory, "config-reader", coven.index_message("config-reader", sequence, expires, "none", entries, self.public(mallory)).decode(), "coven-index", package / f"index-{suffix}"))
        fork(2, "a")
        shutil.copytree(self.root / "consumer/app", self.root / "consumer/fresh")
        (self.root / "consumer/fresh/witchy.lock").unlink()
        self.assertEqual(self.grimoire("lock", "consumer/fresh").returncode, 0)
        # The honest registry back: the older reviewed lock (alice) is refused, since the chain this
        # machine took on first use (alice -> mallory) holds alice and is not continued. The user,
        # told so, forgets the package; the reviewed lock then anchors the honest chain.
        (package / "rotations").write_text(honest[0]); (package / "index").write_text(honest[1])
        (self.app / "witchy.lock").write_text(reviewed)
        self.refused(self.grimoire("run", "consumer/app"), "does not continue the one this machine verified")
        forgot = self.grimoire("trust", "consumer/app", "--forget", "config-reader")
        self.assertIn("its retired keys", forgot.stdout, forgot.stderr)
        ran = self.grimoire("run", "consumer/app")
        self.assertEqual(ran.stdout, "8080\n", ran.stderr)
        fork(3, "b")
        # The fork again: refused for the reviewed lock, which is not relocked onto it, and for a
        # lockless project.
        self.refused(self.grimoire("run", "consumer/app"), "does not continue the one this machine verified")
        self.refused(self.grimoire("lock", "consumer/app"), "does not continue the one this machine verified")
        self.assertEqual((self.app / "witchy.lock").read_text(), reviewed)
        shutil.copytree(self.root / "consumer/fresh", self.root / "consumer/third")
        (self.root / "consumer/third/witchy.lock").unlink()
        self.refused(self.grimoire("lock", "consumer/third"), "nor a key it was handed to")
        forgot = self.grimoire("trust", "consumer/third", "--forget", "config-reader")
        self.assertIn("kept its floor (index 2), its retired keys", forgot.stdout, forgot.stderr)
        (package / "rotations").write_text(honest[0]); (package / "index").write_text(honest[1])
        self.refused(self.grimoire("lock", "consumer/third"), "this machine forgot its owners")
        with (self.root / "consumer/third/witchy.toml").open("a") as manifest:
            manifest.write(f'\n[owners]\nconfig-reader = {{ owner = "{alice_print}", recovery = "none" }}\n')
        relocked = self.grimoire("lock", "consumer/third")
        self.assertEqual(relocked.returncode, 0, relocked.stderr)
        self.assertIn(f'signer = "{self.key_print_of(bob)}"', (self.root / "consumer/third/witchy.lock").read_text())


    def test_an_earlier_stores_conflict_migrates_as_forgotten(self):
        self.use_policy_signatures(packages=True)
        alice, alice_print = self.key("alice")
        self.assertEqual(self.coven_elsewhere("alice", "publish", "publisher/config-reader", "--registry", "registry", "--key", str(alice)).returncode, 0)
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        store = next((self.root / "state/grimoire/trust").glob("*.json"))
        old = json.loads(store.read_text())
        for held in old["packages"].values():
            held.pop("retired"); held.pop("provisional"); held["conflict"] = True
        store.write_text(json.dumps(old))
        self.assertIn("owners (forgotten)", self.grimoire("trust", "consumer/app").stdout)
        (self.app / "witchy.lock").unlink()
        self.refused(self.grimoire("lock", "consumer/app"), "this machine forgot its owners")


    def test_the_lock_refuses_an_older_index_than_it_saw(self):
        """A registry writer serving the package's index from before a revocation is refused."""
        self.use_policy_signatures(packages=True)
        alice, _ = self.key("alice")
        self.publish_reader("0.1.0", alice)
        index = self.root / "registry/config-reader/index"
        before = index.read_text()
        self.assertEqual(self.coven("refresh", "config-reader", "--registry", "registry", "--key", str(alice)).returncode, 0)
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        self.assertIn("index = 2", (self.app / "witchy.lock").read_text())
        index.write_text(before)
        self.refused(self.grimoire("run", "consumer/app"), "refused: the registry serves index 1 of config-reader, older than index 2 already seen")
        self.refused(self.grimoire("lock", "consumer/app"), "older than index 2 already seen")
        index.unlink()
        self.refused(self.grimoire("run", "consumer/app"), "has no index")


    def test_the_app_grants_the_file_not_the_dependency(self):
        self.use_policy_signatures(packages=True)
        self.locked()
        (self.app / "main.grants.toml").write_text("[files]\n")
        self.refused(self.grimoire("run", "consumer/app"), "the document grants none ([files].config)")


class SnapshotRun(RegistryRun):
    """The registry snapshot, signed by the operator's own tool (`coven snapshot`), which no
    publisher's key can sign: every grimoire call here sees exactly the snapshots the operator
    signed (no `stamp`)."""

    def grimoire(self, *args, env=None, first_use=False, stamp=False):
        return super().grimoire(*args, env=env, first_use=first_use, stamp=stamp)

    def min_base(self):
        # The operator states the latest snapshot it signed (Coven's `--min-base`, required after
        # the first): here, the one the registry serves.
        served = self.root / "registry/snapshot"
        return ["--min-base", str(self.load_coven().parse_snapshot(served.read_bytes()).sequence)] if served.is_file() else ["--first"]

    def snapshot(self, key=None):
        latest = self.min_base()
        signed = self.coven_elsewhere("operator", "snapshot", "--registry", "registry", "--key", str(key or self.operator), "--id", "test", *latest)
        self.assertEqual(signed.returncode, 0, signed.stderr)
        return signed

    def start_with_snapshot(self):
        """Branch the real first snapshot of the unsigned example, including the operator's copy."""
        source = locked_project()
        if not source:
            self.publish()
            self.snapshot()
            return
        registry = (self.root / "registry").resolve()
        self.assertEqual(list(registry.iterdir()), [])
        shutil.copytree(source / "registry", registry, dirs_exist_ok=True)
        # The operator stores the signed bytes outside the registry, under its canonical path.
        saved = self.saved_operator_copy()
        saved.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / "operator-copy", saved)

    def saved_operator_copy(self):
        from unittest.mock import patch
        with patch.dict(os.environ, XDG_STATE_HOME=str(self.root / "machine-operator")):
            return self.load_coven().operator_state((self.root / "registry").resolve(), self.operator_print)

    def signed_by(self, who):
        """config-reader 0.1.0 published by `who`, its owner declared in the project."""
        key, fingerprint = self.key(who)
        self.assertEqual(self.coven_elsewhere(who, "publish", "publisher/config-reader", "--registry", "registry", "--key", str(key)).returncode, 0)
        self.declare(f'config-reader = "{fingerprint}"')
        return key

    def test_a_later_version_becomes_resolvable_at_the_next_snapshot(self):
        self.use_policy_signatures(packages=True)
        alice = self.signed_by("alice")
        self.refused(self.grimoire("lock", "consumer/app"), "has no snapshot: its operator signs one")
        self.snapshot()
        self.assertEqual(self.grimoire("lock", "consumer/app").returncode, 0)
        self.publish_reader("0.1.1", alice, "// 0.1.1\n")
        manifest = self.app / "witchy.toml"
        manifest.write_text(re.sub(r'config-reader = "[0-9.]+"', 'config-reader = "0.1.1"', manifest.read_text(), count=1))
        self.refused(self.grimoire("lock", "consumer/app"), "does not list this version")
        self.snapshot()
        relocked = self.grimoire("lock", "consumer/app")
        self.assertEqual(relocked.returncode, 0, relocked.stderr)
        self.assertIn("sequence = 2", (self.app / "witchy.lock").read_text())

    def test_nothing_of_an_older_suite_is_a_pin(self):
        """The witchy-2 profile, no downgrade: a lock pinning a sha256 digest, an SSH fingerprint or
        a witchy-1 key is refused whole with "lock again"; this machine's records of older-suite
        keys are set aside (they anchor nothing); a declaration of an older-suite key is refused;
        and a snapshot signed with ssh-keygen or witchy-1 is refused."""
        self.use_policy_signatures(packages=True)
        alice = self.signed_by("alice")
        alice_print = self.key_print_of(alice)
        self.snapshot()
        locked = self.grimoire("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        lock = self.app / "witchy.lock"
        locked = lock.read_text()
        digest = re.search(r'digest = "(sha384:[0-9a-f]{96})"', locked).group(1)
        old_print, witchy1 = "SHA256:" + "C" * 43, "witchy-1:" + "1" * 64
        for old, new, why in [(digest, "sha256:" + "2" * 64, "is pinned by a sha256 digest"),
                              (f'signer = "{alice_print}"', f'signer = "{old_print}"', "is pinned by an SSH key fingerprint"),
                              (f'operator = "{OPERATOR_ID}"', f'operator = "{witchy1}"', "pins operator witchy-1:")]:
            lock.write_text(locked.replace(old, new))
            for command in ("run", "lock"):
                refused = self.grimoire(command, "consumer/app")
                self.refused(refused, "was made before the witchy-2 profile")
                self.assertIn(why, refused.stderr)
                self.assertIn("Remove it and lock again", refused.stderr)
        lock.write_text(locked)
        # This machine's records from before the profile: set aside as forgotten, the registry
        # identifier kept, so a first use is refused (review of d902ac6) and nothing old anchors.
        store = next((self.root / "state/grimoire/trust").glob("*.json"))
        known = json.loads(store.read_text())
        known["operator"] = {"keys": [witchy1], "retired": [], "snapshot": 7, "provisional": False, "registry": "test"}
        known["packages"]["config-reader"] = {"owners": [old_print], "recovery": "none", "index": 9, "retired": [], "provisional": False}
        store.write_text(json.dumps(known))
        lockless = self.root / "consumer/lockless"
        shutil.copytree(self.app, lockless)
        (lockless / "witchy.lock").unlink()
        undeclared = lockless / "witchy.toml"
        undeclared.write_text(undeclared.read_text().replace(f'operator = "{OPERATOR_ID}"\nid = "test"\n', ""))
        first_use = self.grimoire("lock", "consumer/lockless", first_use=True)
        self.refused(first_use, "this machine forgot the registry's operator")
        self.assertIn("signed before the witchy-2 profile; set aside as forgotten", first_use.stderr)
        ran = self.grimoire("run", "consumer/app")
        self.assertEqual(ran.stdout, "8080\n", ran.stderr)
        self.assertIn(f"operator: {OPERATOR_ID}, registry test, snapshot 1", self.grimoire("trust", "consumer/app").stdout)
        # Declarations of an older suite's keys.
        manifest = self.app / "witchy.toml"
        body = manifest.read_text()
        for pin in (old_print, witchy1):
            manifest.write_text(body.replace(f'operator = "{OPERATOR_ID}"', f'operator = "{pin}"'))
            self.refused(self.grimoire("run", "consumer/app"), f"[registry] operator {pin} is")
        manifest.write_text(body.replace(f'config-reader = "{alice_print}"', f'config-reader = "{old_print}"'))
        self.refused(self.grimoire("run", "consumer/app"), f"[owners] config-reader: {old_print} is an SSH key fingerprint, from before the witchy-2 profile")
        manifest.write_text(body)
        # A snapshot signed with ssh-keygen, or in witchy-1's domain.
        snapshot = self.root / "registry/snapshot"
        good = snapshot.read_bytes()
        snapshot.write_bytes(b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAp7 operator\ncoven-snapshot 1\nregistry test\nsequence 9\n")
        self.refused(self.grimoire("run", "consumer/app", stamp=False), "signed with ssh-keygen, which no role uses any more")
        snapshot.write_bytes(good.replace(b"witchy/v2/", b"witchy/v1/", 1))
        self.refused(self.grimoire("run", "consumer/app", stamp=False), "signed with witchy-1 (ML-DSA-44), which no role uses any more")

    def test_the_operator_is_declared_or_trusted_on_first_use_explicitly(self):
        self.use_policy_signatures(packages=True)
        _, writer_print = self.operator_key("writer")
        manifest = self.app / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{self.operator_print}"\nid = "test"\n', ""))
        self.start_with_snapshot()
        refused = self.grimoire("lock", "consumer/app")
        self.refused(refused, f'grimoire:   [registry]\ngrimoire:   operator = "{self.operator_print}"\ngrimoire:   id = "test"')
        self.assertIn("--trust-first-use", refused.stderr)
        self.assertFalse((self.app / "witchy.lock").exists())
        trusted = self.grimoire("lock", "consumer/app", first_use=True)
        self.assertEqual(trusted.returncode, 0, trusted.stderr)
        self.assertIn(f'its operator {self.operator_print}, registry test', trusted.stderr)
        self.assertIn("(first use)", self.grimoire("trust", "consumer/app").stdout)
        # The lock anchors it now, on any machine.
        ran = self.grimoire("run", "consumer/app", env=dict(os.environ, XDG_STATE_HOME=str(self.root / "machine-other")))
        self.assertEqual(ran.stdout, "8080\n", ran.stderr)
        # A declared operator that did not sign the snapshot is refused, and so is another registry's identifier.
        body = manifest.read_text()
        manifest.write_text(body.replace('[registry]\n', f'[registry]\noperator = "{writer_print}"\nid = "test"\n', 1))
        self.refused(self.grimoire("lock", "consumer/app"), f"its operator is {writer_print}")
        manifest.write_text(body.replace('[registry]\n', f'[registry]\noperator = "{self.operator_print}"\nid = "other"\n', 1))
        self.refused(self.grimoire("lock", "consumer/app"), "different registry identifiers (other, test)")
        (self.app / "witchy.lock").unlink()
        self.refused(self.grimoire("lock", "consumer/app"), "the project names registry other, and this machine verified this registry's snapshots as test")
        elsewhere = dict(os.environ, XDG_STATE_HOME=str(self.root / "machine-third"))
        self.refused(self.grimoire("lock", "consumer/app", env=elsewhere), "signed for registry test, not other")

    def test_a_cloned_projects_operator_pin_never_stands_in_for_a_first_use(self):
        """On a machine with no record, a cloned project pins a writer's operator key, and the
        writer serves a snapshot signed by it. That record never vouches for a later first use
        (`--trust-first-use`), and the user's own project, anchored on the real operator, is
        refused as a fork rather than silently replaced by it, or replacing it (final-state
        review; review of 101f2b4)."""
        self.use_policy_signatures(packages=True)
        import datetime
        writer, writer_print = self.operator_key("writer")
        self.publish()
        self.stamp()
        honest = (self.root / "registry/snapshot").read_bytes()
        coven = self.load_coven()
        entries = coven.parse_snapshot(honest).entries
        later = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0) + datetime.timedelta(hours=1)
        (self.root / "registry/snapshot").write_bytes(self.forge_operator("snapshot", writer, lambda public: coven.snapshot_message("test", 1, later, entries, public)))
        for project, change in [("cloned", lambda text: text.replace(self.operator_print, writer_print)),
                                ("undeclared", lambda text: text.replace(f'operator = "{self.operator_print}"\nid = "test"\n', ""))]:
            shutil.copytree(self.app, self.root / "consumer" / project)
            manifest = self.root / "consumer" / project / "witchy.toml"
            manifest.write_text(change(manifest.read_text()))
        locked = self.grimoire("lock", "consumer/cloned", stamp=False)
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.refused(self.grimoire("lock", "consumer/undeclared", first_use=True, stamp=False), "which does not stand in for this project's")
        (self.root / "registry/snapshot").write_bytes(honest)
        self.refused(self.grimoire("lock", "consumer/app", stamp=False), "a fork or a rollback")


# SnapshotRun shares RegistryRun's fixtures, not its tests.
for _name in dir(RegistryRun):
    if _name.startswith("test_") and _name not in SnapshotRun.__dict__:
        setattr(SnapshotRun, _name, None)


def make_locked_project(destination):
    """One genuine publication, operator snapshot and first lock serve both kinds of test."""
    case = SnapshotRun()
    try:
        case.setUp()
        case.use_policy_signatures(packages=True)
        case.publish()
        case.snapshot()
        registry = case.root / "registry"
        locked = case.grimoire("lock", "consumer/app", first_use=True)
        case.assertEqual(locked.returncode, 0, locked.stderr)
        shutil.copytree(registry, destination / "registry")
        shutil.copy2(case.saved_operator_copy(), destination / "operator-copy")
        shutil.copy2(case.app / "witchy.lock", destination / "witchy.lock")
        saved, = (case.root / "state/grimoire/trust").glob("*.json")
        shutil.copy2(saved, destination / "trust.json")
        snapshot = case.load_coven().parse_snapshot((registry / "snapshot").read_bytes())
        (destination / "stamp.json").write_text(json.dumps([snapshot.sequence, snapshot.entries]))
    finally:
        case.doCleanups()


if os.environ.pop("GRIMOIRE_TEST_PRELOAD", None):
    # In run.py's forking process: the compiler client, release codec and test supervisor that
    # Grimoire and Coven load on first use (each compiled from its verified bytes, keyed by
    # digest), loaded here once, so that no case compiles them again. Each case still has its
    # own copy, and no compiler session is started here.
    _cli = load_cli()
    _cli.compiler_client()
    _cli.release_format()
    _cli.native_tests()
    if inline_tools() is not None:
        _coven = inline_tools().fresh_coven(shutil.which(os.environ.get("COVEN", "coven")))
        if hasattr(_coven, "release_format"):
            _coven.release_format()
    del _cli


if __name__ == "__main__":
    unittest.main()
