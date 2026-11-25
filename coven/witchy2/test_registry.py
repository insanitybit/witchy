"""Checks of the Witchy 2 registry programs, with the configured witchyc and
witchy-wasmtime-host. Run: python3 witchy2/test_registry.py"""

from concurrent.futures import ThreadPoolExecutor
import hashlib
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest

from cli_test_support import fresh_coven, invoke_coven, policy_signatures

HERE = Path(__file__).resolve().parent
COVEN = HERE / "coven"
INITIAL_RELEASE = os.environ.pop("COVEN_TEST_INITIAL_RELEASE", None)


def bundle(source, name="lib", version="1.0.0", requires=""):
    """The bytes a version's digest is of: its `package` record, then its source."""
    return f"witchy-package 1\nname {name}\nversion {version}\n{requires}\n".encode() + source


def tool(variable, name):
    return shutil.which(os.environ.get(variable, name))


class Registry(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        (self.root / "registry").mkdir()
        # The owner's record of the indexes it signed (XDG_STATE_HOME/coven), per test.
        previous = os.environ.get("XDG_STATE_HOME")
        os.environ["XDG_STATE_HOME"] = str(self.root / "state")
        self.addCleanup(lambda: os.environ.pop("XDG_STATE_HOME") if previous is None else os.environ.update(XDG_STATE_HOME=previous))

    def package(self, folder, text, version="1.0.0", name="lib", requires=""):
        package = self.root / folder
        package.mkdir()
        (package / "lib.wy").write_text(text)
        (package / "witchy.toml").write_text(f'[package]\nname = "{name}"\nversion = "{version}"\nsource = "lib.wy"\n'
                                             f'\n[dependencies]\n{requires}')
        return package

    def test_single_signed_metadata_has_a_read_bound(self):
        import io

        cli = self.module()
        root = self.root / "registry"
        for name, read in (
            (cli.INDEX, lambda: cli.read_index(root, "lib")),
            (cli.SNAPSHOT, lambda: cli.verify_snapshot(root, None, None, 0, None)),
            (cli.SIGNATURE, lambda: cli.signer_of(root, "lib", "1.0.0", "sha384:" + "0" * 96)),
        ):
            with self.subTest(name=name):
                with (root / name).open("wb") as stream:
                    stream.truncate(cli.SIGNED_OBJECT_BYTES + 1)
                with self.assertRaisesRegex(cli.CovenError, "signed object exceeds"):
                    read()

        message = cli.domain("index")
        message += b"x" * (cli.MESSAGE_BYTES - len(message) - 1) + b"\n"
        framed = cli.signed_object(message, bytes(cli.SIGNATURE_BYTES))
        self.assertEqual(len(framed), cli.SIGNED_OBJECT_BYTES)
        (root / cli.INDEX).write_bytes(framed)
        self.assertEqual(cli.registry_file(root / cli.INDEX, signed=True), framed)
        self.assertEqual(cli.split_signed(framed, "index", ValueError()), (message, bytes(cli.SIGNATURE_BYTES)))
        stream = io.BytesIO(framed + b"x")
        with self.assertRaisesRegex(cli.CovenError, "signed object exceeds"):
            cli.read_signed_bytes(stream, root / cli.INDEX, size=0)

    def test_rotation_framing_bounds_each_record(self):
        cli = self.module()
        role = "rotation"
        prefix = cli.domain(role)
        message = prefix + b"x" * (cli.MESSAGE_BYTES - len(prefix) - 1) + b"\n"
        signature = bytes(cli.SIGNATURE_BYTES)
        framed = cli.signed_object(message, signature)
        self.assertEqual(len(framed), cli.SIGNED_OBJECT_BYTES)
        self.assertEqual(cli.split_all(framed * 2, role, ValueError()), [(message, signature)] * 2)

        class RecordBytes(bytes):
            def __getitem__(self, key):
                if isinstance(key, slice):
                    if len(range(*key.indices(len(self)))) > cli.SIGNED_OBJECT_BYTES:
                        raise AssertionError("framer copied more than one record")
                return super().__getitem__(key)

        for overlong in (cli.signed_object(b"x" + message, signature), framed[:-1] + b"x\n"):
            with self.assertRaisesRegex(ValueError, "malformed rotation"):
                cli.split_all(RecordBytes(framed + overlong), role, ValueError("malformed rotation"))

    def coven(self, *args, env=None):
        # A fetch verifies the registry's snapshot, or says it does not: these checks of each
        # package's own index fetch without one unless they name the operator.
        if args and args[0] == "fetch" and "--operator" not in args and "--no-snapshot" not in args:
            args = (*args, "--no-snapshot")
        # A pinned operator comes with the registry's identifier: these tests' registry is `test`.
        if args and args[0] in ("fetch", "verify-snapshot") and "--operator" in args and "--registry-id" not in args:
            args = (*args, "--registry-id", "test")
        command = [COVEN, *map(str, args)]
        if self._testMethodName == "test_one_of_two_racing_publishers_wins":
            return subprocess.run(command, cwd=self.root, capture_output=True, text=True, timeout=60, env=env)
        return invoke_coven(command, self.root, env)

    def counted(self):
        """An environment whose engine logs each program it runs (`self.runs()`), then runs it."""
        log = self.root / "engine-runs"
        host = self.root / "counting-host"
        host.write_text(f'#!/bin/sh\nbasename "$1" .wasm >> {log}\nexec {tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")} "$@"\n')
        host.chmod(0o755)
        return dict(os.environ, WITCHY_WASMTIME_HOST=str(host))

    def runs(self):
        """The programs the counting engine ran since the last call, in order."""
        log = self.root / "engine-runs"
        ran = log.read_text().split() if log.exists() else []
        log.unlink(missing_ok=True)
        return ran

    def test_digest_is_the_engines_sha384_at_every_padding_boundary(self):
        """digest.wy, the engine's SHA-384 (AWS-LC), against hashlib, across SHA-384's block
        boundaries (111, 112, 128), up to the most a version holds (1 MiB)."""
        module = self.root / "digest.wasm"
        subprocess.run([tool("WITCHYC", "witchyc"), "build", HERE / "digest.wy", "-o", module, "--heap-pages=24"], check=True,
                       capture_output=True)
        for size in [0, 1, 3, 111, 112, 113, 127, 128, 129, 1000, 4097, 1 << 20]:
            data = os.urandom(size)
            sample = self.root / f"sample{size}"
            sample.write_bytes(data)
            ran = subprocess.run([tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"), module, "--output", "--allocator",
                                  f"--file={sample}", "--memory-pages=64", "--unmetered"], capture_output=True, text=True, check=True)
            self.assertEqual(ran.stdout, f"sha384:{hashlib.sha384(data).hexdigest()}\n", size)

    def test_one_of_two_racing_publishers_wins(self):
        first = self.package("a", "pub fn one() -> U64:\n    return 1\n")
        second = self.package("b", "pub fn one() -> U64:\n    return 2\n")
        with ThreadPoolExecutor(2) as pool:
            results = list(pool.map(lambda p: self.coven("publish", p, "--registry", "registry"), [first, second]))
        self.assertEqual(sorted(r.returncode for r in results), [0, 1], [r.stderr for r in results])
        winner = [first, second][[r.returncode for r in results].index(0)]
        slot = self.root / "registry/lib/1.0.0"
        self.assertEqual((slot / "source.wy").read_bytes(), (winner / "lib.wy").read_bytes())
        self.assertEqual((slot / "package").read_text(), "witchy-package 1\nname lib\nversion 1.0.0\n\n")
        digest = "sha384:" + hashlib.sha384(bundle((winner / "lib.wy").read_bytes())).hexdigest()
        self.assertEqual((slot / "digest").read_text(), digest)

    def test_a_refused_fetch_leaves_nothing_in_the_cache(self):
        self.enterContext(policy_signatures())
        self.assertEqual(self.coven("publish", self.package("a", "pub fn one() -> U64:\n    return 1\n"),
                                    "--registry", "registry").returncode, 0)
        source = self.root / "registry/lib/1.0.0/source.wy"
        good = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "good")
        self.assertEqual(good.returncode, 0, good.stderr)
        digest = good.stdout.split("verified ")[1].split()[0]
        source.write_text(source.read_text().replace("1", "7"))
        for expect in [[], ["--expect", digest]]:
            bad = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "bad", *expect)
            self.assertEqual(bad.returncode, 1)
            self.assertIn("does not match its recorded digest", bad.stderr)
            self.assertEqual(list((self.root / "bad").iterdir()), [])
        (source.parent / "digest").write_text("sha384:" + hashlib.sha384(bundle(source.read_bytes())).hexdigest())
        repinned = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "bad", "--expect", digest)
        self.assertIn("not the locked digest", repinned.stderr)
        self.assertEqual(list((self.root / "bad").iterdir()), [])

    def test_a_version_pins_its_requirements(self):
        self.enterContext(policy_signatures())
        leaf = self.package("leaf", "pub fn one() -> U64:\n    return 1\n", name="leaf")
        top = self.package("top", "pub fn two() -> U64:\n    return 2\n", name="top", requires='leaf = "1.0.0"\n')
        early = self.coven("publish", top, "--registry", "registry")
        self.assertIn("requires leaf 1.0.0, which is not published", early.stderr)
        self.assertFalse((self.root / "registry/top").exists())
        self.assertEqual(self.coven("publish", leaf, "--registry", "registry").returncode, 0)
        published = self.coven("publish", top, "--registry", "registry")
        source = (top / "lib.wy").read_bytes()
        digest = "sha384:" + hashlib.sha384(bundle(source, "top", requires="requires leaf 1.0.0\n")).hexdigest()
        self.assertIn(f"published {digest}", published.stdout, published.stderr)
        fetched = self.coven("fetch", "top", "1.0.0", "--registry", "registry", "--into", "cache", "--expect", digest)
        self.assertEqual(fetched.returncode, 0, fetched.stderr)
        self.assertEqual((self.root / "cache/package").read_text(), "witchy-package 1\nname top\nversion 1.0.0\nrequires leaf 1.0.0\n\n")
        release = self.root / "top.coven"
        release.write_bytes(bundle(source, "top", requires="requires leaf 1.0.0\n"))
        inspected = self.coven("inspect", release)
        self.assertEqual(inspected.stdout, f"top 1.0.0\nrequires leaf 1.0.0\n{digest}\n", inspected.stderr)
        release.write_bytes(b"unframed source")
        refused = self.coven("inspect", release)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(refused.stdout, "")
        self.assertIn("incomplete package record", refused.stderr)
        # Dropping a requirement from the registry's record is a different package.
        record = self.root / "registry/top/1.0.0/package"
        record.write_text("witchy-package 1\nname top\nversion 1.0.0\n\n")
        dropped = self.coven("fetch", "top", "1.0.0", "--registry", "registry", "--into", "bad")
        self.assertIn("does not match its recorded digest", dropped.stderr)
        self.assertEqual(list((self.root / "bad").iterdir()), [])

    def test_publishing_the_same_source_again_completes_a_stopped_publish(self):
        self.enterContext(policy_signatures())
        package = self.package("a", "pub fn one() -> U64:\n    return 1\n")
        self.assertEqual(self.coven("publish", package, "--registry", "registry").returncode, 0)
        slot = self.root / "registry/lib/1.0.0"
        digest = "sha384:" + hashlib.sha384(bundle((package / "lib.wy").read_bytes())).hexdigest()
        self.assertEqual((slot / "digest").read_text(), digest)
        self.assertIn(f"unchanged {digest}", self.coven("publish", package, "--registry", "registry").stdout)
        # A publish that stopped after its claim: the digest is there and the source is not.
        (slot / "source.wy").unlink()
        broken = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "early")
        self.assertEqual(broken.returncode, 1)
        other = self.package("b", "pub fn one() -> U64:\n    return 2\n")
        self.assertIn("different source", self.coven("publish", other, "--registry", "registry").stderr)
        self.assertFalse((slot / "source.wy").exists())
        self.assertEqual((slot / "digest").read_text(), digest)
        again = self.coven("publish", package, "--registry", "registry")
        self.assertIn("unchanged sha384:", again.stdout, again.stderr)
        self.assertEqual((slot / "source.wy").read_bytes(), (package / "lib.wy").read_bytes())
        self.assertEqual(self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "late").returncode, 0)

    def test_fetch_worker_checks_release_before_writing_cache(self):
        cli = self.module()
        record, body = bundle(b"fn main():\n    return\n").split(b"\n\n", 1)
        record += b"\n\n"
        digest = "sha384:" + hashlib.sha384(record + body).hexdigest()
        slot, cache = self.root.resolve() / "registry", self.root.resolve() / "cache"
        cache.mkdir()
        (slot / "digest").write_text(digest)
        with tempfile.TemporaryDirectory() as folder:
            temp = Path(folder).resolve()
            expected = temp / "expected"
            grants = {"dirs": {role: "{ root = %s }" % cli.toml_string(path)
                                for role, path in (("slot", slot), ("cache", cache))},
                      "files": {"expected": cli.toml_string(expected)}}
            for package, source, selected, authorized, message in (
                (record[:-1], b"\n" + body, "lib", digest, "not a release this tool builds from"),
                (record, body, "other", digest, "not a release this tool builds from"),
                (record, body, "lib", "sha384:" + "0" * 96, "record changed during the fetch"),
            ):
                with self.subTest(message=message, selected=selected):
                    (slot / "package").write_bytes(package)
                    (slot / "source.wy").write_bytes(source)
                    expected.write_text(f"{selected}\n1.0.0\n{authorized}\n")
                    self.assertIn(message, cli.run_program("fetch", grants, temp))
                    self.assertEqual(list(cache.iterdir()), [])
            expected.write_text(f"lib\n1.0.0\n{digest}\n")
            self.assertEqual(cli.run_program("fetch", grants, temp), "verified " + digest)
            self.assertEqual((cache / "package").read_bytes(), record)
            self.assertEqual((cache / "source.wy").read_bytes(), body)

            # A multi-file release checks the content digests before replacing the cache.
            codec = cli.release_format()
            files = {"lib.wy": b'import "value.wy"\n', "value.wy": b"pub fn value() -> U64:\n    return 4\n"}
            multi = codec.pack("lib", "1.0.0", {}, "lib.wy", files)
            multi_record, multi_body = codec.split(multi)
            (slot / "package").write_bytes(multi_record)
            (slot / "source.wy").write_bytes(multi_body.replace(b"return 4", b"return 9"))
            corrupted = codec.digest_of((slot / "package").read_bytes() + (slot / "source.wy").read_bytes())
            (slot / "digest").write_text(corrupted)
            expected.write_text(f"lib\n1.0.0\n{corrupted}\n")
            self.assertIn("not a release this tool builds from", cli.run_program("fetch", grants, temp))
            self.assertEqual((cache / "package").read_bytes(), record)
            self.assertEqual((cache / "source.wy").read_bytes(), body)

            exact = codec.digest_of(multi)
            (slot / "source.wy").write_bytes(multi_body)
            (slot / "digest").write_text(exact)
            expected.write_text(f"lib\n1.0.0\n{exact}\n{exact}")
            self.assertEqual(cli.run_program("fetch", grants, temp), "verified " + exact)
            self.assertEqual((cache / "package").read_bytes(), multi_record)
            self.assertEqual((cache / "source.wy").read_bytes(), multi_body)

    def test_publish_and_fetch_transfer_the_largest_package(self):
        limit = 1 << 20
        start, end = "//", "\npub fn one() -> U64:\n    return 1\n"
        size = limit - len(bundle(b"")) - len(start) - len(end)
        text = start + "x" * size + end
        payload = bundle(text.encode())
        self.assertEqual(len(payload), limit)
        digest = "sha384:" + hashlib.sha384(payload).hexdigest()

        package = self.package("max", text)
        published = self.coven("publish", package, "--registry", "registry")
        self.assertEqual(published.returncode, 0, published.stderr)
        self.assertIn(f"published {digest}", published.stdout)
        repeated = self.coven("publish", package, "--registry", "registry")
        self.assertEqual(repeated.returncode, 0, repeated.stderr)
        self.assertIn(f"unchanged {digest}", repeated.stdout)
        fetched = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--expect", digest, "--into", "out")
        self.assertEqual(fetched.returncode, 0, fetched.stderr)
        self.assertEqual((self.root / "out/source.wy").read_bytes(), text.encode())

        oversized = self.package("too-large", text + "\n", version="1.0.1")
        refused = self.coven("publish", oversized, "--registry", "registry")
        self.assertEqual(refused.returncode, 1)
        self.assertIn("more than a version holds", refused.stderr)
        self.assertFalse((self.root / "registry/lib/1.0.1/digest").exists())

        (self.root / "registry/lib/1.0.0/source.wy").write_text(text + "\n")
        refused = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--expect", digest, "--into", "bad")
        self.assertEqual(refused.returncode, 1)
        self.assertIn("larger than a version holds", refused.stderr)
        self.assertFalse((self.root / "bad/source.wy").exists())

    def test_the_source_is_a_file_of_the_package(self):
        secret = self.root / "secret.wy"
        secret.write_text("pub fn key() -> U64:\n    return 42\n")
        package = self.package("a", "pub fn one() -> U64:\n    return 1\n")
        (package / "link.wy").symlink_to(secret)
        manifest = package / "witchy.toml"
        for source in ["../secret.wy", str(secret), "link.wy"]:
            manifest.write_text(f'[package]\nname = "lib"\nversion = "1.0.0"\nsource = "{source}"\n')
            refused = self.coven("publish", package, "--registry", "registry")
            self.assertEqual(refused.returncode, 1, source)
            self.assertFalse((self.root / "registry/lib/1.0.0/source.wy").exists(), source)

    def test_a_symlinked_slot_is_refused(self):
        victim = self.root / "victim"
        victim.mkdir()
        (victim / "source.wy").write_text("precious\n")
        (self.root / "registry/lib").mkdir()
        (self.root / "registry/lib/1.0.0").symlink_to(victim)
        package = self.package("a", "pub fn one() -> U64:\n    return 1\n")
        refused = self.coven("publish", package, "--registry", "registry")
        self.assertIn("is not a directory of the registry", refused.stderr)
        self.assertEqual(sorted(p.name for p in victim.iterdir()), ["source.wy"])
        self.assertEqual((victim / "source.wy").read_text(), "precious\n")
        fetched = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "c")
        self.assertIn("is not a directory of the registry", fetched.stderr)
        (self.root / "registry/lib/1.0.0").unlink()
        (self.root / "registry/lib").rmdir()
        (self.root / "registry/lib").symlink_to(victim)
        self.assertIn("is not a directory of the registry", self.coven("publish", package, "--registry", "registry").stderr)

    def test_the_engine_refuses_a_link_the_launcher_did_not_see(self):
        """The race the launcher's own check cannot close: the slot becomes a link after it."""
        import importlib.machinery, importlib.util
        loader = importlib.machinery.SourceFileLoader("coven_launcher", str(COVEN))
        coven = importlib.util.module_from_spec(importlib.util.spec_from_loader("coven_launcher", loader))
        loader.exec_module(coven)
        victim = (self.root / "victim").resolve()
        victim.mkdir()
        (self.root / "link").symlink_to(victim)
        expected = self.root.resolve() / "expected"
        expected.write_text("")
        with self.assertRaises(coven.CovenError) as refused:
            coven.run_program("fetch", {"dirs": {"slot": "{ root = %s }" % coven.toml_string(self.root.resolve() / "link"),
                                                 "cache": "{ root = %s }" % coven.toml_string(victim)},
                                        "files": {"expected": coven.toml_string(expected)}}, self.root.resolve())
        self.assertIn("without following a link", str(refused.exception))

    def test_a_module_cache_others_can_write_is_not_used(self):
        from unittest.mock import patch
        cli = self.module()
        cache = self.root / "cache"
        modules = cache / "coven/modules"
        modules.mkdir(parents=True)
        with patch.dict(os.environ, XDG_CACHE_HOME=str(cache)):
            for mode, usable in [(0o777, False), (0o700, True)]:
                modules.chmod(mode)
                with cli._cache_directory() as held:
                    self.assertEqual(held is not None, usable)
                    cli._cache_put(held, "fixture.wasm", b"fixture")
                    self.assertEqual(cli._cache_get(held, "fixture.wasm"), b"fixture" if usable else None)
                self.assertEqual((modules / "fixture.wasm").exists(), usable)

    def test_an_unbound_source_graph_does_not_write_cache_entries(self):
        from unittest.mock import patch

        cli = self.module()
        client = cli.compiler_client()
        cache = self.root / "cache"
        shutil.copyfile(HERE / "coven_permissions.py", self.root / "coven_permissions.py")
        source = self.root / "verify.wy"
        nested = 'import "nested/source.wy"\n'
        source.write_text(nested)
        wrapper = self.root / "compiler"
        wrapper.write_text("#!/bin/sh\nexit 1\n")
        wrapper.chmod(0o700)
        builds = []
        grants = []

        def compile_request(args, **_):
            if args[1] == "build":
                builds.append(args[2])
                Path(args[4]).write_bytes(f"build {len(builds)}".encode())
                return subprocess.CompletedProcess(args, 0, "", "")
            grants.append(Path(args[3]).read_bytes())
            return subprocess.CompletedProcess(args, 0, "--output\n", "")

        with patch.object(cli, "HERE", self.root), \
                patch.dict(os.environ, XDG_CACHE_HOME=str(cache), WITCHYC=str(wrapper)), \
                patch.object(cli, "compiler_client", return_value=client), \
                patch.object(client, "run", side_effect=compile_request):
            flat = "fn main():\n    return\n"
            for number, text in enumerate((nested, flat), 1):
                source.write_text(text)
                self.assertEqual(cli.witchy_module("verify").read_bytes(), f"build {number}".encode())
                self.assertEqual(cli.grant_flags("verify", self.root / "grants.toml", b""), ["--output"])
            self.assertFalse(cache.exists())
            if sys.platform != "darwin":
                return
            # Switching back to a compiler session must use the artifact for its
            # graph even when the intervening unbound build overwrote the scratch file.
            from types import SimpleNamespace
            session = SimpleNamespace(identity=client.executable_identity(wrapper.stat()),
                                      request=lambda _: (0, "fixture-graph", "", ""))
            source.write_text(flat)
            with patch.object(client, "selected", return_value=session):
                self.assertEqual(cli.witchy_module("verify").read_bytes(), b"build 3")
            source.write_text(nested)
            self.assertEqual(cli.witchy_module("verify").read_bytes(), b"build 4")
            source.write_text(flat)
            with patch.object(client, "selected", return_value=session):
                self.assertEqual(cli.witchy_module("verify").read_bytes(), b"build 3")
        self.assertEqual(len(builds), 4)
        self.assertEqual(grants, [b"", b""])

    def test_a_path_like_a_placeholder_gets_the_grant_the_real_document_does(self):
        """The grant check runs on the document with each path replaced by `/__coven_path_N__`.
        A real path holding another placeholder's text must not be read as that placeholder when
        the paths are put back (review of 7d40fde: a write grant on a path no check named)."""
        coven = self.module()
        for name in ["slot", "__coven_path_3__", "expected"]:
            (self.root / name).mkdir()
        lines = ["[input]", f'input = "{self.root / "batch"}"',
                 "[dirs]", f'slot = {{ root = "{self.root / "slot"}" }}', f'cache = {{ root = "{self.root / "__coven_path_3__"}" }}',
                 "[files]", f'expected = "{self.root / "expected" / "file"}"']
        real = self.root / "real.toml"
        real.write_text("\n".join(["[paths]", "links = false", *lines]) + "\n")
        checked = subprocess.run([tool("WITCHYC", "witchyc"), "wasm-grants", HERE / "fetch.wy", real], capture_output=True, text=True, check=True)
        temp = self.root / "temp"
        temp.mkdir()
        self.assertEqual(coven.witchy_flags("fetch", lines, temp), checked.stdout.splitlines())

    def test_a_cache_with_an_access_list_or_a_fifo_is_not_used(self):
        """An access control list lets others write in the cache whatever its mode says, and its
        entries inherit it; a FIFO under an entry's name would hang the read (review of 7d40fde)."""
        coven = self.module()
        cache = self.root / "cache"
        modules = cache / "coven" / "modules"
        modules.mkdir(parents=True, mode=0o700)
        previous = os.environ.get("XDG_CACHE_HOME")
        os.environ["XDG_CACHE_HOME"] = str(cache)
        self.addCleanup(lambda: os.environ.pop("XDG_CACHE_HOME") if previous is None else os.environ.update(XDG_CACHE_HOME=previous))
        os.mkfifo(modules / "planted.flags", 0o600)
        with coven._cache_directory() as held:
            self.assertIsNotNone(held)
            with ThreadPoolExecutor(1) as pool:
                read = pool.submit(coven._cache_get, held, "planted.flags")
                try:
                    self.assertIsNone(read.result(timeout=5))
                finally:
                    if not read.done():  # unblock the hung open, so the test can end
                        os.close(os.open(modules / "planted.flags", os.O_WRONLY | os.O_NONBLOCK))
            coven._cache_put(held, "own.flags", b"--allocator\n")
            self.assertEqual(coven._cache_get(held, "own.flags"), b"--allocator\n")
            os.link(modules / "own.flags", modules / "linked.flags")
            self.assertIsNone(coven._cache_get(held, "own.flags"))
        if sys.platform != "darwin":
            return
        subprocess.run(["chmod", "+a", "everyone allow list,add_file,search,delete_child,file_inherit,directory_inherit", modules], check=True)
        with coven._cache_directory() as held:
            self.assertIsNone(held)

    def test_pins_keys_and_objects_of_older_suites_are_refused(self):
        """No downgrade: an SSH fingerprint, a witchy-1 key ID or a sha256 digest pins nothing, an
        SSH or witchy-1 public key is no key, and a registry's object signed with ssh-keygen or
        witchy-1, or a version recorded with a sha256 digest, is refused with what to do."""
        self.enterContext(policy_signatures(packages=True))
        alice, alice_id = self.key("alice")
        self.start_with_alices_release(alice)
        fetch = lambda *more: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out", *more)
        fingerprint, witchy1, sha256 = "SHA256:" + "A" * 43, "witchy-1:" + "0" * 64, "sha256:" + "0" * 64
        for flag, value, why in [("--expect-signer", fingerprint, "is an SSH key fingerprint, from before the witchy-2 profile"),
                                 ("--expect-recovery", witchy1, "is a witchy-1 key ID, from before the witchy-2 profile"),
                                 ("--expect", sha256, "is a sha256 digest, from before the witchy-2 profile (SHA-384 only): lock again"),
                                 ("--operator", witchy1, "is a witchy-1 key ID")]:
            self.assertIn(why, fetch(flag, value).stderr, flag)
        (self.root / "ssh.pub").write_text("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAp7 rescue\n")
        refused = self.coven("publish", self.package("b", "pub fn two() -> U64:\n    return 2\n", name="other"), "--registry", "registry",
                             "--key", alice, "--recovery-key", "ssh.pub")
        self.assertIn("is an SSH public key, from before the witchy-2 profile", refused.stderr)
        # The registry's objects from before the profile: refused, never verified another way.
        slot = self.root / "registry/lib/1.0.0"
        good = {name: (slot.parent / name if name == "index" else slot / name).read_bytes() for name in ["index", "release-signature", "digest"]}
        (slot / "release-signature").write_bytes(b"ssh-ed25519 AAAA alice\n-----BEGIN SSH SIGNATURE-----\nx\n-----END SSH SIGNATURE-----\n")
        self.assertIn("release signature is signed with ssh-keygen, from before the witchy-2 profile", fetch().stderr)
        (slot / "release-signature").write_bytes(good["release-signature"].replace(b"witchy/v2/", b"witchy/v1/", 1))
        self.assertIn("release signature is signed with witchy-1 (ML-DSA-44)", fetch().stderr)
        (slot / "release-signature").write_bytes(good["release-signature"])
        (slot.parent / "index").write_bytes(good["index"].replace(b"witchy/v2/", b"witchy/v1/", 1))
        self.assertIn("its index is signed with witchy-1", fetch().stderr)
        (slot.parent / "index").write_bytes(good["index"])
        (slot / "digest").write_text(sha256)
        self.assertIn("recorded digest is a sha256 digest, from before the witchy-2 profile (SHA-384 only): this registry predates", fetch().stderr)
        self.assertIn("recorded digest is a sha256 digest", self.coven("publish", self.package("c", "pub fn one() -> U64:\n    return 1\n"),
                                                                       "--registry", "registry", "--key", alice).stderr)
        (slot / "digest").write_bytes(good["digest"])
        self.assertEqual(fetch("--expect-signer", alice_id).returncode, 0)

    def key(self, name):
        """A witchy-2 key: a 32-byte seed only its owner reads, its public key beside it (`coven
        key`), and its key ID."""
        return self.operator(name)

    def sign_object(self, role, key, message):
        """`message` signed with `key` outside Coven's checks (a registry writer's key, or a stolen
        one), as Coven writes a signed object."""
        coven = self.module()
        return coven.signed_object(message, coven.witchy_sign(role, coven.signing_key(key), message))

    def test_a_release_is_signed_once_and_verified_on_fetch(self):
        (alice, alice_print), (mallory, _) = self.key("alice"), self.key("mallory")
        package = self.package("a", "pub fn one() -> U64:\n    return 1\n")
        published = self.coven("publish", package, "--registry", "registry", "--key", alice)
        self.assertIn(f"signed by {alice_print}", published.stdout, published.stderr)
        fetched = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "c")
        self.assertIn(f"lib 1.0.0: signed by {alice_print}", fetched.stdout, fetched.stderr)
        # The same release, signed again by another key: the package's owner stays.
        again = self.coven("publish", package, "--registry", "registry", "--key", mallory)
        self.assertIn(f"owned by {alice_print}", again.stderr)
        # A signature changed, or moved from another version, does not verify; nothing is kept.
        slot = self.root / "registry/lib/1.0.0"
        signature = (slot / "release-signature").read_text()
        other = self.package("b", "pub fn one() -> U64:\n    return 2\n", version="1.0.1")
        self.assertEqual(self.coven("publish", other, "--registry", "registry", "--key", alice).returncode, 0)
        for forged in [signature.replace("A", "B", 1), (self.root / "registry/lib/1.0.1/release-signature").read_text()]:
            (slot / "release-signature").write_text(forged)
            refused = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "bad")
            self.assertIn("does not verify", refused.stderr)
            self.assertFalse((self.root / "bad").exists() and any((self.root / "bad").iterdir()))
        # The guest has now accepted the genuine release and refused both forgeries.
        # Exercise signer pinning and unsigned-owner policy with independent verification.
        self.enterContext(policy_signatures(packages=True))
        # A consumer that pins the signer refuses another key, or none.
        (slot / "release-signature").write_text(signature)
        pinned = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "e", "--expect-signer", alice_print)
        self.assertEqual(pinned.returncode, 0, pinned.stderr)
        _, mallory_print = self.key("mallory2")
        self.assertIn(f"not by the expected {mallory_print}", self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "f", "--expect-signer", mallory_print).stderr)
        self.assertFalse((self.root / "f").exists() and any((self.root / "f").iterdir()))
        # A signature removed from a version of an owned package does not make it unsigned-and-fine.
        (slot / "release-signature").unlink()
        self.assertIn(f"unsigned, and lib is owned by {alice_print}", self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "g").stderr)
        # An unsigned version of a package with no index says so.
        self.assertEqual(self.coven("publish", self.package("u", "pub fn one() -> U64:\n    return 1\n", name="free"), "--registry", "registry").returncode, 0)
        self.assertIn("free 1.0.0: unsigned", self.coven("fetch", "free", "1.0.0", "--registry", "registry", "--into", "d").stdout)
        self.assertIn(f"unsigned, not by the expected {alice_print}", self.coven("fetch", "free", "1.0.0", "--registry", "registry", "--into", "h", "--expect-signer", alice_print).stderr)

    def test_a_signature_planted_in_an_empty_slot_claims_nothing(self):
        self.enterContext(policy_signatures())
        (alice, alice_print), (mallory, _) = self.key("alice"), self.key("mallory")
        text = "pub fn one() -> U64:\n    return 1\n"
        package = self.package("a", text)
        # A registry writer predicts the digest and signs it first, with its own key.
        digest = "sha384:" + hashlib.sha384(bundle(text.encode())).hexdigest()
        slot = self.root / "registry/lib/1.0.0"
        slot.mkdir(parents=True)
        message = self.module().release_message("lib", "1.0.0", digest, self.public(mallory))
        (slot / "release-signature").write_bytes(self.sign_object("release", mallory, message))
        # The publish is refused before it stores anything, signed or not.
        refused = self.coven("publish", package, "--registry", "registry")
        self.assertIn("holds files but no published version", refused.stderr)
        self.assertIn(f"already signed by", self.coven("publish", package, "--registry", "registry", "--key", alice).stderr)
        self.assertEqual(sorted(path.name for path in slot.iterdir()), ["release-signature"])
        # A signed publish that stopped after signing leaves only its own signature: it resumes.
        (slot / "release-signature").unlink()
        alice_only = self.coven("publish", package, "--registry", "registry", "--key", alice)
        self.assertEqual(alice_only.returncode, 0, alice_only.stderr)
        for stored in ["digest", "package", "source.wy"]:
            (slot / stored).unlink()
        resumed = self.coven("publish", package, "--registry", "registry", "--key", alice)
        self.assertIn(f"signed by {alice_print}", resumed.stdout, resumed.stderr)

    def test_only_the_releases_signer_can_withdraw_it(self):
        self.enterContext(policy_signatures(packages=True))
        (alice, _), (mallory, mallory_print) = self.key("alice"), self.key("mallory")
        self.start_with_alices_release(alice)
        fetch = lambda into: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", into)
        self.assertIn("only its key can change its index", self.coven("yank", "lib", "1.0.0", "--registry", "registry", "--reason", "x", "--key", mallory).stderr)
        self.assertEqual(self.coven("yank", "lib", "1.0.0", "--registry", "registry", "--reason", "use 1.0.1", "--key", alice).returncode, 0)
        # Yanked: refused to a new resolution, given (and reported) against a locked digest.
        self.assertIn("refused: yanked by its publisher: use 1.0.1", fetch("a").stderr)
        digest = (self.root / "registry/lib/1.0.0/digest").read_text()
        locked = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "a2", "--expect", digest)
        self.assertIn("lib 1.0.0: yanked: use 1.0.1", locked.stdout, locked.stderr)
        self.assertEqual(self.coven("revoke", "lib", "1.0.0", "--registry", "registry", "--reason", "CVE-0000", "--key", alice).returncode, 0)
        revoked = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "b", "--expect", digest)
        self.assertIn("refused: revoked by its publisher: CVE-0000", revoked.stderr)
        self.assertFalse((self.root / "b").exists() and any((self.root / "b").iterdir()))
        # The index records the state: a yank cannot undo a revocation, and only the owner signs it.
        self.assertIn("not undone by a yank", self.coven("yank", "lib", "1.0.0", "--registry", "registry", "--reason", "x", "--key", alice).stderr)
        index = self.root / "registry/lib/index"
        good = index.read_text()
        index.write_text(good.replace("CVE-0000", "CVE-0001"))
        self.assertIn("does not verify", fetch("c").stderr)
        self.assertFalse((self.root / "c").exists() and any((self.root / "c").iterdir()))

    def index_of(self, name="lib"):
        return (self.root / "registry" / name / "index").read_text()

    def test_the_index_refuses_rollback_deletion_and_unlisted_versions(self):
        self.enterContext(policy_signatures(packages=True))
        (alice, alice_print), (mallory, _) = self.key("alice"), self.key("mallory")
        self.start_with_alices_release(alice)
        before_yank = self.index_of()
        self.assertEqual(self.coven("yank", "lib", "1.0.0", "--registry", "registry", "--reason", "use 1.0.1", "--key", alice).returncode, 0)
        digest = (self.root / "registry/lib/1.0.0/digest").read_text()
        fetch = lambda into, *more: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", into, "--expect", digest, *more)
        seen = fetch("a")
        self.assertIn("lib 1.0.0: index 2", seen.stdout, seen.stderr)
        current = self.index_of()
        # A registry writer restores the index from before the yank: a client that saw index 2
        # refuses it; one that never did is the freeze case, bounded by the index's expiry.
        index = self.root / "registry/lib/index"
        index.write_text(before_yank)
        self.assertIn("older than index 2 already seen", fetch("b", "--min-sequence", "2").stderr)
        self.assertFalse((self.root / "b").exists())
        # Deleting the index hides nothing: a signed version needs one.
        index.unlink()
        self.assertIn("has no index", fetch("c").stderr)
        self.assertIn("has no index", fetch("c", "--min-sequence", "2").stderr)
        index.write_text(current)
        evil = self.package("e", "pub fn one() -> U64:\n    return 666\n", version="9.9.9", name="lib")
        # Mallory cannot publish a signed version of alice's package, and nothing is stored ...
        self.assertIn(f"owned by {alice_print}", self.coven("publish", evil, "--registry", "registry", "--key", mallory).stderr)
        self.assertFalse((self.root / "registry/lib/9.9.9").exists() and any((self.root / "registry/lib/9.9.9").iterdir()))
        # ... nor an unsigned one; and a version a registry writer stores around Coven is not listed.
        self.assertIn(f"owned by {alice_print}", self.coven("publish", evil, "--registry", "registry").stderr)
        shutil.rmtree(self.root / "registry/lib/9.9.9", ignore_errors=True)
        shutil.copytree(self.root / "registry/lib/1.0.0", self.root / "registry/lib/9.9.9")
        (self.root / "registry/lib/9.9.9/release-signature").unlink()
        refused = self.coven("fetch", "lib", "9.9.9", "--registry", "registry", "--into", "d")
        self.assertIn("does not list this version", refused.stderr)
        self.assertFalse((self.root / "d").exists())

    def test_the_owners_copy_repairs_an_index_put_back_deleted_or_replaced(self):
        self.enterContext(policy_signatures(packages=True))
        (alice, alice_print), (mallory, _) = self.key("alice"), self.key("mallory")
        self.start_with_alices_release(alice)
        before = self.index_of()
        self.assertEqual(self.coven("revoke", "lib", "1.0.0", "--registry", "registry", "--reason", "CVE-1", "--key", alice).returncode, 0)
        index = self.root / "registry/lib/index"
        fetch = lambda into: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", into, "--min-sequence", "2", "--expect-signer", alice_print)
        # A registry writer puts back the index from before the revocation. The owner's routine
        # refresh signs from its own copy, not the registry's: the revocation stays.
        index.write_text(before)
        refreshed = self.coven("refresh", "lib", "--registry", "registry", "--key", alice)
        self.assertIn("does not serve the index 2 of lib this key last signed", refreshed.stderr)
        self.assertIn("version 1.0.0 sha384:", self.index_of())
        self.assertIn("revoked CVE-1", self.index_of())
        self.assertIn("refused: revoked by its publisher: CVE-1", fetch("a").stderr)
        # Deleted, or replaced by an index under another key: the owner's next update restores it.
        index.unlink()
        newer = self.package("b", "pub fn one() -> U64:\n    return 2\n", version="1.0.1")
        published = self.coven("publish", newer, "--registry", "registry", "--key", alice)
        self.assertEqual(published.returncode, 0, published.stderr)
        self.assertIn("revoked CVE-1", self.index_of())
        self.assertIn("version 1.0.1", self.index_of())
        import datetime
        coven = self.module()
        index.write_bytes(self.sign_object("index", mallory, coven.index_message("lib", 999, datetime.date.today() + datetime.timedelta(days=300), "none", {}, self.public(mallory))))
        self.assertIn("refused", fetch("b").stderr)
        self.assertEqual(self.coven("refresh", "lib", "--registry", "registry", "--key", alice).returncode, 0)
        self.assertIn("revoked CVE-1", self.index_of())
        self.assertIn(f"\nsigner {Path(f'{alice}.pub').read_text().strip()}\n", self.index_of())
        # Without the owner's copy, another key cannot start an index over alice's signed versions.
        index.unlink()
        shutil.rmtree(self.root / "registry/lib/1.0.1")
        self.assertIn("another key cannot start", self.coven("publish", newer, "--registry", "registry", "--key", mallory).stderr)
        self.assertFalse((self.root / "registry/lib/1.0.1").exists() and any((self.root / "registry/lib/1.0.1").iterdir()))

    def test_a_lost_index_is_not_restarted_over_signed_versions(self):
        self.enterContext(policy_signatures(packages=True))
        alice, _ = self.key("alice")
        self.start_with_alices_release(alice)
        self.assertEqual(self.coven("revoke", "lib", "1.0.0", "--registry", "registry", "--reason", "CVE-1", "--key", alice).returncode, 0)
        index = self.root / "registry/lib/index"
        saved = index.read_text()
        # The owner on a new machine (no copy), and the registry's index deleted: a new index
        # would list 1.0.0 as active again. Refused; restoring the last index signed works.
        os.environ["XDG_STATE_HOME"] = str(self.root / "new-machine")
        index.unlink()
        newer = self.package("b", "pub fn one() -> U64:\n    return 2\n", version="1.0.1")
        for refused in [self.coven("publish", newer, "--registry", "registry", "--key", alice), self.coven("refresh", "lib", "--registry", "registry", "--key", alice)]:
            self.assertIn("the registry lost it", refused.stderr)
        self.assertFalse(index.exists())
        index.write_text(saved)
        self.assertEqual(self.coven("publish", newer, "--registry", "registry", "--key", alice).returncode, 0)
        self.assertIn("revoked CVE-1", self.index_of())

    def test_rotation_hands_a_package_to_a_new_key(self):
        self.enterContext(policy_signatures(packages=True))
        (alice, alice_print), (bob, bob_print), (rescue, rescue_print), (mallory, mallory_print) = \
            self.key("alice"), self.key("bob"), self.key("rescue"), self.key("mallory")
        self.start_with_alices_release(alice, rescue)
        fetch = lambda version, into, *pins: self.coven("fetch", "lib", version, "--registry", "registry", "--into", into, *pins)
        self.assertIn(f"lib 1.0.0: recovery {rescue_print}", fetch("1.0.0", "a").stdout)
        # With a recovery key, the owner key alone cannot hand the package on (a thief could).
        self.assertIn(f"only by its recovery key {rescue_print}", self.coven("rotate", "lib", "--registry", "registry", "--key", alice, "--new-key", mallory).stderr)
        self.assertFalse((self.root / "registry/lib/rotations").exists())
        # The recovery key hands it to bob (alice's key lost): old versions stay valid, bob publishes.
        self.assertIn("give --min-sequence", self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--from-owner", alice_print).stderr)
        self.assertIn("older than index 5", self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--from-owner", alice_print, "--min-sequence", "5").stderr)
        rotated = self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--from-owner", alice_print, "--min-sequence", "1")
        self.assertIn(f"owner {alice_print} -> {bob_print} (index 2)", rotated.stdout, rotated.stderr)
        pinned = fetch("1.0.0", "b", "--expect-signer", alice_print, "--min-sequence", "1", "--expect-recovery", rescue_print)
        self.assertIn(f"lib 1.0.0: owner {bob_print}", pinned.stdout, pinned.stderr)
        self.assertIn(f"lib 1.0.0: signed by {alice_print}", pinned.stdout)
        newer = self.package("pkg-1.0.1", "pub fn one() -> U64:\n    return 2\n", version="1.0.1")
        self.assertIn(f"handed from this key to {bob_print}", self.coven("publish", newer, "--registry", "registry", "--key", alice).stderr)
        self.assertIn(f"handed from this key to {bob_print}", self.coven("refresh", "lib", "--registry", "registry", "--key", alice).stderr)
        self.assertEqual(self.coven("publish", newer, "--registry", "registry", "--key", bob).returncode, 0)
        self.assertIn("needs the pinned recovery key", fetch("1.0.1", "c", "--expect-signer", alice_print).stderr)
        self.assertIn(f"lib 1.0.1: signed by {bob_print}", fetch("1.0.1", "c", "--expect-signer", alice_print, "--expect-recovery", rescue_print).stdout)
        # A pin to a key the package never had, or another recovery key, is refused.
        self.assertIn(f"not by the expected {mallory_print}", fetch("1.0.1", "d", "--expect-signer", mallory_print, "--expect-recovery", rescue_print).stderr)
        self.assertIn(f"not the expected {mallory_print}", fetch("1.0.1", "d", "--expect-recovery", mallory_print).stderr)
        # The rotations removed: alice's versions are no longer bob's package's, and a pin to alice
        # no longer reaches bob.
        rotations = self.root / "registry/lib/rotations"
        good = rotations.read_text()
        rotations.unlink()
        self.assertIn(f"signed by {alice_print}, and lib is owned by {bob_print}", fetch("1.0.0", "e").stderr)
        self.assertIn(f"not by the expected {alice_print}", fetch("1.0.1", "e", "--expect-signer", alice_print, "--expect-recovery", rescue_print).stderr)
        # Rewritten to hand the package to mallory instead: the signature does not verify.
        mallory_public = Path(f"{mallory}.pub").read_text().split()[1]
        rotations.write_text(good.replace(Path(f"{bob}.pub").read_text().split()[1], mallory_public))
        self.assertIn("does not verify", fetch("1.0.1", "f").stderr)
        # A statement signed by the old owner rather than the recovery key: refused.
        rotations.write_bytes(self.sign_object("rotation", alice, self.module().rotation_message("lib", alice_print, self.public(bob), self.public(alice))))
        self.assertIn(f"not by the package's recovery key {rescue_print}", fetch("1.0.1", "g").stderr)
        for into in "efg":
            self.assertFalse((self.root / into).exists() and any((self.root / into).iterdir()))
        rotations.write_text(good)
        self.assertEqual(fetch("1.0.1", "h", "--expect-signer", alice_print, "--expect-recovery", rescue_print).returncode, 0)

    def test_without_a_recovery_key_the_owner_rotates(self):
        self.enterContext(policy_signatures(packages=True))
        (alice, alice_print), (bob, bob_print), (mallory, _) = self.key("alice"), self.key("bob"), self.key("mallory")
        self.start_with_alices_release(alice)
        self.assertIn("this key is not lib's owner, so it rotates as lib's recovery key (by the registry's index, none)", self.coven("rotate", "lib", "--registry", "registry", "--key", mallory, "--new-key", bob).stderr)
        self.assertIn(f"owner {alice_print} -> {bob_print}", self.coven("rotate", "lib", "--registry", "registry", "--key", alice, "--new-key", bob, "--no-snapshot").stdout)
        self.assertIn("has owned lib before", self.coven("rotate", "lib", "--registry", "registry", "--key", bob, "--new-key", alice, "--no-snapshot").stderr)
        fetched = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "a", "--expect-signer", alice_print, "--expect-recovery", "none")
        self.assertIn(f"lib 1.0.0: owner {bob_print}", fetched.stdout, fetched.stderr)

    def test_a_chain_signed_by_a_recovery_key_the_attacker_named_is_refused(self):
        self.enterContext(policy_signatures(packages=True))
        (alice, alice_print), (rescue, rescue_print), (evil, evil_print) = self.key("alice"), self.key("rescue"), self.key("evil")
        self.start_with_alices_release(alice, rescue)
        # A registry writer signs, with their own key, an index naming that key as the recovery key,
        # and a rotation from alice to it "by the recovery key".
        import datetime
        coven = self.module()
        (self.root / "registry/lib/rotations").write_bytes(self.sign_object("rotation", evil, coven.rotation_message("lib", alice_print, self.public(evil), self.public(evil))))
        entries = coven.read_index(self.root / "registry/lib", "lib").entries
        forged_index = coven.index_message("lib", 50, datetime.date.today() + datetime.timedelta(days=30), evil_print, entries, self.public(evil))
        (self.root / "registry/lib/index").write_bytes(self.sign_object("index", evil, forged_index))
        fetch = lambda into, *pins: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", into, *pins)
        self.assertIn("needs the pinned recovery key", fetch("a", "--expect-signer", alice_print).stderr)
        forged = fetch("b", "--expect-signer", alice_print, "--expect-recovery", rescue_print)
        self.assertNotEqual(forged.returncode, 0)
        self.assertIn(f"recovery key is {evil_print}, not the expected {rescue_print}", forged.stderr)
        # The owner is not locked out: its copy names rescue, the forged chain does not verify
        # against it, and the next update repairs the index.
        repaired = self.coven("refresh", "lib", "--registry", "registry", "--key", alice)
        self.assertEqual(repaired.returncode, 0, repaired.stderr)
        self.assertIn(f"\nsigner {Path(f'{alice}.pub').read_text().strip()}\n", self.index_of())
        (self.root / "registry/lib/rotations").unlink()
        self.assertEqual(fetch("c", "--expect-signer", alice_print, "--expect-recovery", rescue_print).returncode, 0)

    def signed_index(self, key, sequence, expires, recovery, entries, file):
        import importlib.machinery, importlib.util
        loader = importlib.machinery.SourceFileLoader("coven_module", str(COVEN))
        coven = importlib.util.module_from_spec(importlib.util.spec_from_loader("coven_module", loader))
        loader.exec_module(coven)
        file.write_bytes(self.sign_object("index", key, coven.index_message("lib", sequence, expires, recovery, entries, self.public(key))))
        return coven

    def test_the_recovery_key_takes_back_a_package_from_a_stolen_owner_key(self):
        self.enterContext(policy_signatures(packages=True))
        import datetime
        keys = {who: self.key(who) for who in ["alice", "rescue", "bob", "thief"]}
        (alice, alice_print), (rescue, rescue_print), (bob, bob_print), (thief, _) = keys["alice"], keys["rescue"], keys["bob"], keys["thief"]
        machine = lambda who: os.environ.update(XDG_STATE_HOME=str(self.root / f"state-{who}"))
        machine("alice")
        self.start_with_alices_release(alice, rescue)
        alice_copy = next((self.root / "state-alice/coven/signed").iterdir())
        # The thief, with alice's key: a version of its own, the recovery key signed away, and the
        # package handed to the thief by Coven's own rotate.
        machine("thief")
        self.assertEqual(self.coven("publish", self.package("t", "pub fn one() -> U64:\n    return 666\n", version="1.0.2"), "--registry", "registry", "--key", alice).returncode, 0)
        index = self.root / "registry/lib/index"
        coven = self.signed_index(alice, 3, datetime.date.today() + datetime.timedelta(days=30), "none",
                                  self.read_entries(), index)
        machine("thief-2")
        self.assertEqual(self.coven("rotate", "lib", "--registry", "registry", "--key", alice, "--new-key", thief, "--no-snapshot").returncode, 0)
        pins = ["--expect-signer", alice_print, "--expect-recovery", rescue_print]
        fetch = lambda version, into, *more: self.coven("fetch", "lib", version, "--registry", "registry", "--into", into, *pins, *more)
        self.assertNotEqual(fetch("1.0.2", "a").returncode, 0)
        # The recovery key rotates to bob, from alice's last copy: the thief's rotation is dropped,
        # the recovery key restored, and the index signed above every sequence served.
        machine("rescue")
        self.assertIn("give --from-index", self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--from-owner", alice_print, "--min-sequence", "4").stderr)
        rotated = self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--from-owner", alice_print, "--min-sequence", "4", "--from-index", alice_copy)
        self.assertIn(f"owner {alice_print} -> {bob_print} (index 5)", rotated.stdout, rotated.stderr)
        self.assertIn("dropping 1 rotation(s)", rotated.stderr)
        recovered = fetch("1.0.0", "b", "--min-sequence", "4")
        self.assertIn(f"lib 1.0.0: owner {bob_print}", recovered.stdout, recovered.stderr)
        self.assertIn(f"lib 1.0.0: recovery {rescue_print}", recovered.stdout)
        self.assertIn("does not list this version", fetch("1.0.2", "c").stderr)
        # The new owner withdraws a version an earlier key signed.
        machine("bob")
        self.assertEqual(self.coven("revoke", "lib", "1.0.0", "--registry", "registry", "--reason", "rebuilt", "--key", bob).returncode, 0)
        self.assertIn("revoked by its publisher: rebuilt", fetch("1.0.0", "d").stderr)
        # Alice's own machine sees the package handed on and does not undo it.
        machine("alice")
        self.assertIn(f"handed from this key to {bob_print}", self.coven("refresh", "lib", "--registry", "registry", "--key", alice).stderr)

    def test_the_recovery_key_rotates_only_from_the_owner_it_names(self):
        self.enterContext(policy_signatures(packages=True))
        import datetime
        (alice, alice_print), (rescue, _), (bob, _), (writer, _) = self.key("alice"), self.key("rescue"), self.key("bob"), self.key("writer")
        self.start_with_alices_release(alice, rescue)
        # A registry writer, with only its own key, plants an index under that key naming the
        # real recovery key, so the recovery key would hand the package on from the writer.
        self.signed_index(writer, 5, datetime.date.today() + datetime.timedelta(days=30), self.key_print(rescue), self.read_entries(), self.root / "registry/lib/index")
        os.environ["XDG_STATE_HOME"] = str(self.root / "state-rescue")
        self.assertIn("give --from-owner", self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--min-sequence", "1").stderr)
        refused = self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--min-sequence", "1", "--from-owner", alice_print)
        self.assertIn(f"rotate from is signed by {self.key_print(writer)}", refused.stderr)
        self.assertFalse((self.root / "registry/lib/rotations").exists())

    def read_entries(self):
        import importlib.machinery, importlib.util
        loader = importlib.machinery.SourceFileLoader("coven_module", str(COVEN))
        coven = importlib.util.module_from_spec(importlib.util.spec_from_loader("coven_module", loader))
        loader.exec_module(coven)
        return coven.read_index(self.root / "registry/lib", "lib").entries

    def test_the_recovery_key_does_not_rotate_from_an_expired_index(self):
        self.enterContext(policy_signatures(packages=True))
        import datetime
        (alice, alice_print), (rescue, _), (bob, _) = self.key("alice"), self.key("rescue"), self.key("bob")
        self.start_with_alices_release(alice, rescue)
        self.signed_index(alice, 9, datetime.date.today() - datetime.timedelta(days=3), f"{self.key_print(rescue)}", self.read_entries(), self.root / "registry/lib/index")
        os.environ["XDG_STATE_HOME"] = str(self.root / "state-rescue")
        self.assertIn("expired on", self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--from-owner", alice_print, "--min-sequence", "1").stderr)

    def key_print(self, key):
        return self.module().signing_key(key).id

    def test_an_expired_index_is_refused_until_its_owner_refreshes_it(self):
        self.enterContext(policy_signatures(packages=True))
        alice, _ = self.key("alice")
        self.start_with_alices_release(alice)
        # The owner signs an index that expired yesterday (as an old one served by a frozen registry).
        import datetime
        coven = self.module()
        index = self.root / "registry/lib/index"
        entries = coven.read_index(index.parent, "lib").entries
        index.write_bytes(self.sign_object("index", alice, coven.index_message("lib", 7, datetime.date.today() - datetime.timedelta(days=2), "none", entries, self.public(alice))))
        expired = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "a")
        self.assertIn("expired on", expired.stderr)
        refreshed = self.coven("refresh", "lib", "--registry", "registry", "--key", alice, "--valid-days", "7")
        self.assertIn("lib: index 8, valid for 7 days", refreshed.stdout, refreshed.stderr)
        self.assertIn("lib 1.0.0: index 8", self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "b").stdout)

    # The registry snapshot: the operator's key, which no publisher holds, signs which index of
    # every package is current.

    def module(self):
        return fresh_coven(COVEN)

    def snapshot_entries(self):
        return self.module().parse_snapshot((self.root / "registry/snapshot").read_bytes()).entries

    def operator(self, name):
        """A witchy-2 key: a 32-byte seed only its owner reads, its public key beside it (`coven
        key`), and its key ID. The tests' usual names come from test-keys/ (synthetic, fixed, so
        that no test spends an engine run deriving them); any other is made fresh."""
        seed = self.root / name
        if (HERE / "test-keys" / name).is_file():
            import base64
            seed.write_bytes((HERE / "test-keys" / name).read_bytes())
            seed.chmod(0o600)
            public = (HERE / "test-keys" / f"{name}.pub").read_text()
            Path(f"{seed}.pub").write_text(public)
            return seed, "witchy-2:" + hashlib.sha384(base64.b64decode(public.split()[1])).hexdigest()
        seed.write_bytes(os.urandom(32))
        seed.chmod(0o600)
        made = self.coven("key", "--key", seed)
        self.assertEqual(made.returncode, 0, made.stderr)
        return seed, made.stdout.split()[1]

    def forge(self, role, key, message):
        """`message` signed with the operator key `key` outside Coven's checks (a registry writer's
        key, or a stolen one), as Coven writes a signed object."""
        coven = self.module()
        return coven.signed_object(message, coven.witchy_sign(role, coven.signing_key(key), message))

    def public(self, key):
        return self.module().signing_key(key).public

    def signed_snapshot(self, key, sequence, expires, entries, registry_id="test"):
        """A snapshot signed by `key` as Coven writes it (a registry writer's, or a stolen key's)."""
        return self.forge("snapshot", key, self.module().snapshot_message(registry_id, sequence, expires, entries, self.public(key)))

    def published(self, folder, *args, **package):
        result = self.coven("publish", self.package(folder, f"pub fn one() -> U64:\n    return {len(folder)}\n", **package), "--registry", "registry", *args)
        self.assertEqual(result.returncode, 0, result.stderr)

    def start_with_alices_release(self, alice, recovery=None):
        """Branch a common publication (with `recovery`, the recovery key's) into a new registry and
        owner state for this case."""
        if INITIAL_RELEASE is None:
            self.published("a", "--key", alice, *(["--recovery-key", f"{recovery}.pub"] if recovery else []))
            return
        # The fixture is alice's, with rescue as the recovery key (review of the test-speed port).
        self.assertEqual((Path(alice).name, recovery and Path(recovery).name), ("alice", recovery and "rescue"))
        source = Path(INITIAL_RELEASE) / ("recovery" if recovery else ".")
        shutil.copytree(source / "a", self.root / "a")
        registry = self.root / "registry"
        self.assertEqual(list(registry.iterdir()), [])
        shutil.copytree(source / "lib", registry / "lib")
        # Owner state is keyed by the registry's resolved path, as Coven reads it. Install the
        # original signed bytes under the new registry's identity, on this test's machine.
        saved = self.module().signed_state(registry.resolve() / "lib", "lib", self.key_print(alice))
        saved.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / "owner-index", saved)

    def start_with_snapshot(self, alice, operator):
        """Alice's release, and the operator's first snapshot of it, branched as above: the
        registry's snapshot and index copy, and the operator's own copy on this test's machine."""
        self.start_with_alices_release(alice)
        if INITIAL_RELEASE is None:
            self.snapshot(operator)
            return
        self.assertEqual(Path(operator).name, "operator")  # the key that signed the fixture's snapshot
        root = (self.root / "registry").resolve()
        source = Path(INITIAL_RELEASE)
        shutil.copy2(source / "snapshot", root / "snapshot")
        shutil.copytree(source / "indexes", root / "lib/indexes")
        saved = self.module().operator_state(root, self.key_print(operator))
        saved.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / "operator-copy", saved)

    def latest_signed(self, key, registry="registry"):
        """The highest snapshot sequence the operator `key` knows it signed (`--min-base`): in these
        tests, the higher of this machine's copy and the registry's snapshot when this key signed
        it (a writer's or a predecessor's is no statement of this operator's). 0: none."""
        coven, known = self.module(), [0]
        ours = self.key_print(key)
        root = (self.root / registry).resolve()
        for path in (root / "snapshot", coven.operator_state(root, ours)):
            if path.is_file():
                held = coven.parse_snapshot(path.read_bytes())
                if held.signer == ours:
                    known.append(held.sequence)
        return max(known)

    def rotate_operator(self, key, new, *more, registry="registry"):
        return self.coven("rotate-operator", "--registry", registry, "--key", key, "--new-key", new, "--min-base", str(self.latest_signed(key, registry)), *more)

    def snapshot(self, key, *more, registry="registry", registry_id="test"):
        if "--min-base" not in more:
            latest = self.latest_signed(key, registry)
            more = (*more, "--min-base", str(latest)) if latest else (*more, "--first")
        result = self.coven("snapshot", "--registry", registry, "--key", key, "--id", registry_id, *more)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_a_publish_reaches_clients_with_the_next_snapshot(self):
        """Publishers sign without the operator key; a client sees their changes once the operator
        signs them into a snapshot, and until then the earlier versions stay fetchable."""
        self.enterContext(policy_signatures(packages=True))
        (alice, _), (operator, operator_print) = self.key("alice"), self.operator("operator")
        fetch = lambda version, into, *more, name="lib": self.coven("fetch", name, version, "--registry", "registry", "--into", into, "--operator", operator_print, *more)
        self.start_with_alices_release(alice)
        self.assertIn("has no snapshot", fetch("1.0.0", "out-a").stderr)
        self.assertIn("give --operator", subprocess.run([COVEN, "fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-a"], cwd=self.root, capture_output=True, text=True).stderr)
        self.snapshot(operator)
        self.assertIn("lib 1.0.0: snapshot 1", fetch("1.0.0", "out-b").stdout)
        self.assertEqual((self.root / "registry/lib/indexes/1").read_bytes(), (self.root / "registry/lib/index").read_bytes())
        # A new version, and a yank, signed by the publisher alone: not yet published to clients.
        self.published("c", "--key", alice, version="1.0.1")
        self.assertEqual(self.coven("yank", "lib", "1.0.0", "--registry", "registry", "--reason", "old", "--key", alice).returncode, 0)
        self.assertIn("does not list this version", fetch("1.0.1", "out-c").stderr)
        self.assertEqual(fetch("1.0.0", "out-d").returncode, 0)
        self.published("free", name="free")
        self.assertIn("free is not in the registry's snapshot 1: not published yet", fetch("1.0.0", "out-e", name="free").stderr)
        self.snapshot(operator)
        self.assertEqual(fetch("1.0.1", "out-f").returncode, 0, fetch("1.0.1", "out-f").stderr)
        self.assertIn("refused: yanked by its publisher: old", fetch("1.0.0", "out-g").stderr)
        unsigned = fetch("1.0.0", "out-h", name="free")
        self.assertIn("free 1.0.0: unsigned", unsigned.stdout, unsigned.stderr)
        self.assertEqual(self.snapshot_entries()["free"], None)

    def test_an_index_the_snapshot_does_not_name_is_refused(self):
        """Mix and match: the registry serves, under the current snapshot, an older index that its
        owner did sign. Without the snapshot's digest, a client that never saw the newer index takes it."""
        self.enterContext(policy_signatures(packages=True))
        (alice, _), (operator, operator_print) = self.key("alice"), self.operator("operator")
        self.start_with_alices_release(alice)
        self.published("b", "--key", alice, name="other")
        self.snapshot(operator)
        old = (self.root / "registry/other/indexes/1").read_bytes()
        self.assertEqual(self.coven("revoke", "other", "1.0.0", "--registry", "registry", "--reason", "CVE-2", "--key", alice).returncode, 0)
        self.snapshot(operator)
        for path in ["registry/other/indexes/2", "registry/other/index"]:
            (self.root / path).write_bytes(old)
        mixed = self.coven("fetch", "other", "1.0.0", "--registry", "registry", "--into", "out-a", "--operator", operator_print)
        self.assertNotEqual(mixed.returncode, 0)
        self.assertIn("serves another index 2 than its snapshot 2 names", mixed.stderr)
        self.assertFalse((self.root / "out-a").exists() and any((self.root / "out-a").iterdir()))
        self.assertEqual(self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-b", "--operator", operator_print).returncode, 0)
        # Two packages' updates, each inadmissible on its own, in the one signed snapshot: lib
        # handed to mallory by a chain from bob, which never owned it (no recovery key, so the key
        # handing over signs); other's index signed by its owner but naming a recovery key it never
        # had. Both keep their entries, and nothing else changes.
        import datetime
        entries, coven = self.snapshot_entries(), self.module()
        (bob, bob_print), (mallory, mallory_print), (rescue, rescue_print) = self.key("bob"), self.key("mallory"), self.key("rescue")
        later = datetime.date.today() + datetime.timedelta(days=30)
        (self.root / "registry/lib/rotations").write_bytes(self.sign_object("rotation", bob, coven.rotation_message("lib", bob_print, self.public(mallory), self.public(bob))))
        (self.root / "registry/lib/index").write_bytes(self.sign_object("index", mallory, coven.index_message("lib", 99, later, "none", self.read_entries(), self.public(mallory))))
        other = coven.read_index(self.root / "registry/other", "other").entries
        (self.root / "registry/other/index").write_bytes(self.sign_object("index", alice, coven.index_message("other", 3, later, rescue_print, other, self.public(alice))))
        kept = self.snapshot(operator)
        alice_print = self.key_print(alice)
        self.assertIn(f"lib: kept as in snapshot 2: its index 99 is signed by {mallory_print}, to which {alice_print} did not hand it", kept.stderr)
        self.assertIn(f"other: kept as in snapshot 2: its index 3 changes the recovery key from none to {rescue_print}", kept.stderr)
        self.assertEqual(self.snapshot_entries(), entries)

    def test_each_command_verifies_in_the_engine_run_that_acts(self):
        """Every signature a command's decision rests on is verified by the program that acts on
        it, in the same engine run, before it acts: a fetch is one run (fetch.wy), a snapshot or a
        yank one (sign.wy), and verify-snapshot one (verify.wy). A signed publication first
        decodes its manifest in a read-only worker, then runs sign, publish and sign."""
        (alice, _), (operator, operator_print) = self.key("alice"), self.operator("operator")
        seen = self.root / "environments"
        env = self.counted()
        for name, executable in (("WITCHYC", "witchyc"), ("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")):
            real = shutil.which(env.get(name, executable), path=env.get("PATH"))
            self.assertIsNotNone(real, f"cannot find {executable}")
            wrapper = self.root / (name + "-env")
            wrapper.write_text(f"#!/bin/sh\necho {name} >> {shlex.quote(str(seen))}\nenv >> {shlex.quote(str(seen))}\n"
                               f'exec {shlex.quote(real)} "$@"\n')
            wrapper.chmod(0o755)
            env[name] = str(wrapper)
        env.update(REPO_TOKEN="canary-7f3a9", WITCHY_SECRET_X="canary-7f3a9")
        package = self.package("a", "pub fn one() -> U64:\n    return 1\n")
        for args, programs in [(("publish", package, "--registry", "registry", "--key", alice), ["read_package_manifest", "sign", "publish", "sign"]),
                               (("snapshot", "--registry", "registry", "--key", operator, "--id", "test", "--first"), ["sign"]),
                               (("verify-snapshot", "--registry", "registry", "--operator", operator_print, "--keep", "kept"), ["verify"]),
                               (("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out", "--operator", operator_print, "--snapshot", "kept"), ["fetch"]),
                               (("yank", "lib", "1.0.0", "--registry", "registry", "--reason", "old", "--key", alice), ["sign"]),
                               (("refresh", "lib", "--registry", "registry", "--key", alice), ["sign"])]:
            if args[0] in ("publish", "fetch"):
                flags = ["--registry-id", "test"] if args[0] == "fetch" else []
                done = subprocess.run([COVEN, *map(str, args), *flags], cwd=self.root, capture_output=True,
                                      text=True, env=env, timeout=60)
            else:
                done = self.coven(*args, env=env)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual(self.runs(), programs, args[0])
        # The index the registry serves now (yanked, refreshed) is not the one the kept snapshot
        # names: a broken signature on it is not this fetch's to verify, and holds nothing up.
        index = self.root / "registry/lib/index"
        raw = index.read_bytes()
        at = raw.rindex(b"signature ") + 20
        index.write_bytes(raw[:at] + (b"A" if raw[at:at + 1] != b"A" else b"B") + raw[at + 1:])
        done = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "again", "--operator", operator_print, "--snapshot", "kept", env=env)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.runs(), ["fetch"])

        recorded = seen.read_text()
        self.assertTrue({"WITCHYC", "WITCHY_WASMTIME_HOST"}.issubset(recorded.splitlines()))
        self.assertIn("PATH=", recorded)
        self.assertNotIn("canary-7f3a9", recorded)
        self.assertNotIn("WITCHY_SECRET_X", recorded)

    def test_a_fetch_whose_signature_does_not_verify_reports_and_writes_nothing(self):
        """fetch.wy verifies first and reads nothing when a signature does not verify; nothing the
        decision printed from it is reported, and the directory made for the fetch is gone."""
        (alice, _), (operator, operator_print) = self.key("alice"), self.operator("operator")
        self.start_with_snapshot(alice, operator)
        good = (self.root / "registry/snapshot").read_bytes()
        at = good.rindex(b"signature ") + 20
        (self.root / "forged").write_bytes(good[:at] + (b"A" if good[at:at + 1] != b"A" else b"B") + good[at + 1:])
        env = self.counted()
        refused = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "new/out", "--operator", operator_print,
                             "--snapshot", "forged", env=env)
        self.assertEqual(refused.returncode, 1)
        self.assertIn("the registry's snapshot's signature does not verify", refused.stderr)
        self.assertEqual(refused.stdout, "")  # not even `snapshot 1`
        self.assertFalse((self.root / "new").exists())
        self.assertEqual(self.runs()[0], "fetch")
        # A release signature that does not verify, under a snapshot that does.
        signature = self.root / "registry/lib/1.0.0/release-signature"
        raw = signature.read_bytes()
        at = raw.rindex(b"signature ") + 20
        signature.write_bytes(raw[:at] + (b"A" if raw[at:at + 1] != b"A" else b"B") + raw[at + 1:])
        refused = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "new/out", "--operator", operator_print)
        self.assertEqual(refused.returncode, 1)
        self.assertIn("the release signature does not verify", refused.stderr)
        self.assertEqual(refused.stdout, "lib 1.0.0: snapshot 1\n")  # the snapshot, verified, as before
        self.assertFalse((self.root / "new").exists())

    def test_an_engine_run_that_stops_leaves_nothing_unverified(self):
        """A run that stops before its verdicts leaves its batch deferred: it is verified before the
        command reports anything, and if that cannot be done either, only the error is reported,
        never what was decided from a signature not verified (automated review of d0aabae)."""
        (alice, _), (operator, operator_print) = self.key("alice"), self.operator("operator")
        self.start_with_snapshot(alice, operator)
        good = (self.root / "registry/snapshot").read_bytes()
        at = good.rindex(b"signature ") + 20
        (self.root / "forged").write_bytes(good[:at] + (b"A" if good[at:at + 1] != b"A" else b"B") + good[at + 1:])
        real = tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")
        for stopping, why in [("fetch", "the registry's snapshot's signature does not verify"), ("fetch|verify", "stopped")]:
            host = self.root / "stopping-host"
            host.write_text(f'#!/bin/sh\ncase "$(basename "$1" .wasm)" in {stopping}) exit 1;; esac\nexec {real} "$@"\n')
            host.chmod(0o755)
            refused = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out", "--operator", operator_print,
                                 "--snapshot", "forged", env=dict(os.environ, WITCHY_WASMTIME_HOST=str(host)))
            self.assertEqual(refused.returncode, 1)
            self.assertIn(why, refused.stderr)
            self.assertEqual(refused.stdout, "", stopping)  # not `snapshot 1`, from the forged snapshot

    def test_a_batch_above_the_hosts_default_input_is_verified_in_the_run(self):
        """A program's input is granted as many bytes as its batch (the host's default is 64 KiB):
        a validly signed index of 600 versions (about 80 KB) is fetched, in one run (review of
        d0aabae: every batch above 64 KiB stopped the command)."""
        import datetime
        alice, _ = self.key("alice")
        self.start_with_alices_release(alice)
        coven = self.module()
        index = coven.read_index(self.root / "registry/lib", "lib")
        entries = dict(index.entries)
        for number in range(600):
            entries[f"0.{number}.0"] = ("sha384:" + hashlib.sha384(str(number).encode()).hexdigest(), "active", "")
        key = coven.signing_key(alice)
        message = coven.index_message("lib", 2, datetime.date.today() + datetime.timedelta(days=30), index.recovery, entries, key.public)
        (self.root / "registry/lib/index").write_bytes(coven.signed_object(message, coven.witchy_sign("index", key, message)))
        self.assertGreater(len(message), 1 << 16)
        env = self.counted()
        fetched = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out", env=env)
        self.assertEqual(fetched.returncode, 0, fetched.stderr)
        self.assertEqual(self.runs(), ["fetch"])
        self.assertEqual(sorted(path.name for path in (self.root / "out").iterdir()), ["package", "source.wy"])

    def test_an_expired_snapshot_is_refused(self):
        """Freeze: a registry writer keeps serving a snapshot, validly signed, after it expired."""
        self.enterContext(policy_signatures(packages=True))
        import datetime
        (alice, _), (operator, operator_print) = self.key("alice"), self.operator("operator")
        self.start_with_alices_release(alice)
        self.snapshot(operator, "--valid-hours", "1")
        expired = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0) - datetime.timedelta(seconds=1)
        (self.root / "registry/snapshot").write_bytes(self.signed_snapshot(operator, 2, expired, self.snapshot_entries()))
        refused = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-a", "--operator", operator_print)
        self.assertIn("snapshot 2 expired at", refused.stderr)
        self.assertFalse((self.root / "out-a").exists())
        self.assertIn("expired at", self.coven("verify-snapshot", "--registry", "registry", "--operator", operator_print).stderr)
        # The operator signs the next one, above the expired one its key signed.
        self.assertIn("snapshot 3 of registry test, valid for", self.snapshot(operator).stdout)
        self.assertEqual(self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-b", "--operator", operator_print).returncode, 0)
        self.assertIn("1 to 744 hours", self.coven("snapshot", "--registry", "registry", "--key", operator, "--valid-hours", "0", "--min-base", str(self.latest_signed(operator))).stderr)

    def test_a_client_that_saw_a_newer_snapshot_refuses_an_older_one(self):
        """The registry puts back an earlier, unexpired snapshot, hiding a revocation and a new
        package: refused by a client whose floor is the newer snapshot."""
        self.enterContext(policy_signatures(packages=True))
        (alice, _), (operator, operator_print) = self.key("alice"), self.operator("operator")
        self.start_with_snapshot(alice, operator)
        earlier = (self.root / "registry/snapshot").read_bytes()
        self.assertEqual(self.coven("revoke", "lib", "1.0.0", "--registry", "registry", "--reason", "CVE-3", "--key", alice).returncode, 0)
        self.snapshot(operator)
        seen = self.coven("verify-snapshot", "--registry", "registry", "--operator", operator_print)
        self.assertIn("snapshot 2", seen.stdout, seen.stderr)
        (self.root / "registry/snapshot").write_bytes(earlier)
        fetch = lambda into, *more: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", into, "--operator", operator_print, *more)
        self.assertIn("serves snapshot 1, older than snapshot 2 already seen", fetch("out-a", "--min-snapshot", "2").stderr)
        self.assertFalse((self.root / "out-a").exists())
        self.assertIn("older than snapshot 2", self.coven("verify-snapshot", "--registry", "registry", "--operator", operator_print, "--min-sequence", "2").stderr)
        # A client that never saw snapshot 2 is the freeze case, bounded by the snapshot's expiry.
        self.assertEqual(fetch("out-b").returncode, 0)

    def test_the_operator_signs_only_what_an_owner_moved_to(self):
        """A registry writer who puts back an older index, plants one under its own key, or puts
        back an older snapshot does not get it signed: the operator keeps the entry it had, and
        builds on its own copy of its last snapshot."""
        self.enterContext(policy_signatures(packages=True))
        import datetime
        (alice, _), (mallory, _), (operator, operator_print) = self.key("alice"), self.key("mallory"), self.operator("operator")
        self.start_with_snapshot(alice, operator)
        first_snapshot, first_index = (self.root / "registry/snapshot").read_bytes(), (self.root / "registry/lib/index").read_bytes()
        self.assertEqual(self.coven("revoke", "lib", "1.0.0", "--registry", "registry", "--reason", "CVE-4", "--key", alice).returncode, 0)
        self.snapshot(operator)
        fetch = lambda into: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", into, "--operator", operator_print)
        (self.root / "registry/lib/index").write_bytes(first_index)
        self.assertIn("lib: kept as in snapshot 2: it serves index 1, not newer than index 2", self.snapshot(operator).stderr)
        self.assertIn("revoked by its publisher: CVE-4", fetch("out-a").stderr)
        planted = self.root / "planted"
        self.signed_index(mallory, 99, datetime.date.today() + datetime.timedelta(days=30), "none", {}, planted)
        (self.root / "registry/lib/index").write_bytes(planted.read_bytes())
        self.assertIn("but its rotations hand it to no one", self.snapshot(operator).stderr)
        self.assertEqual(self.snapshot_entries()["lib"][0], 2)
        self.assertIn("revoked by its publisher: CVE-4", fetch("out-b").stderr)
        # The owner's next update, from its own copy, is admitted.
        self.assertEqual(self.coven("refresh", "lib", "--registry", "registry", "--key", alice).returncode, 0)
        self.snapshot(operator)
        self.assertEqual(self.snapshot_entries()["lib"][0], 3)
        # An older snapshot put back is refused, never signed over (adversarial review of 363f952);
        # the operator restores its latest, and the next one continues from it.
        latest = (self.root / "registry/snapshot").read_bytes()
        (self.root / "registry/snapshot").write_bytes(first_snapshot)
        self.assertIn("serves snapshot 1 signed by this key, older than this machine's copy 5",
                      self.coven("snapshot", "--registry", "registry", "--key", operator).stderr)
        (self.root / "registry/snapshot").write_bytes(latest)
        continued = self.snapshot(operator)
        self.assertIn("snapshot 6 of registry test", continued.stdout)
        self.assertEqual(self.snapshot_entries()["lib"][0], 3)


    def test_a_stolen_owner_key_cannot_sign_the_recovery_key_out(self):
        """A thief with alice's key signs indexes far ahead (at 10**18-1, at 2**53-1, expiring in
        9999) so that the recovery rotation could never sign above them: none is admitted, one
        within bounds is, and the recovery rotation is admitted over it, also while the thief
        races it under the old key. The snapshot binds the rotations it was admitted through, so a
        writer who removes them changes nothing a client is served (final-state review)."""
        self.enterContext(policy_signatures(packages=True))
        import datetime
        keys = {w: self.key(w) for w in ["alice", "rescue", "bob"]}
        (alice, alice_print), (rescue, rescue_print), (bob, bob_print) = keys["alice"], keys["rescue"], keys["bob"]
        operator, operator_print = self.operator("operator")
        machine = lambda who: os.environ.update(XDG_STATE_HOME=str(self.root / f"state-{who}"))
        machine("alice")
        self.start_with_alices_release(alice, rescue)
        alice_copy = next((self.root / "state-alice/coven/signed").iterdir())
        machine("op")
        self.snapshot(operator)
        machine("thief")
        self.published("t", "--key", alice, version="1.0.2")
        index, entries = self.root / "registry/lib/index", self.read_entries()
        step, today = self.module().MAX_SEQUENCE_STEP, datetime.date.today()
        machine("op")
        # Within bounds, the thief's index is admitted: 1.0.2 is served under alice's pins.
        self.signed_index(alice, 1 + step, today + datetime.timedelta(days=366), rescue_print, entries, index)
        self.snapshot(operator)
        first_snapshot = (self.root / "registry/snapshot").read_bytes()
        pins = ["--operator", operator_print, "--expect-signer", alice_print, "--expect-recovery", rescue_print]
        fetch = lambda version, into: self.coven("fetch", "lib", version, "--registry", "registry", "--into", into, *pins)
        self.assertEqual(fetch("1.0.2", "out-a").returncode, 0)
        # The recovery key hands lib to bob, from alice's last honest copy; the thief signs again
        # under alice's key, racing it: an owner the rotations hand on signs no more.
        machine("rescue")
        rotated = self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--from-owner", alice_print,
                             "--min-sequence", str(1 + step), "--from-index", alice_copy)
        self.assertIn(f"owner {alice_print} -> {bob_print} (index {2 + step})", rotated.stdout, rotated.stderr)
        self.signed_index(alice, 3 + step, today + datetime.timedelta(days=30), rescue_print, entries, index)
        machine("op")
        self.assertIn(f"which its rotations hand on to {bob_print}", self.snapshot(operator).stderr)
        # The thief then removes the rotations: the operator recorded the hand-off it saw (review
        # of 9710ad0: read from the registry alone, it was forgotten and alice's index admitted).
        rotations = self.root / "registry/lib/rotations"
        saved = rotations.read_bytes()
        rotations.unlink()
        # On another of the operator's machines too: the record travels in the signed snapshot
        # (review of 9cb1718: a per-machine record was read as empty there).
        machine("op-2")
        self.assertIn(f"signed by {alice_print}, which this operator saw hand lib on", self.snapshot(operator).stderr)
        machine("op")
        self.assertEqual(self.snapshot_entries()["lib"][0], 1 + step)
        rotations.write_bytes(saved)
        machine("rescue")
        self.assertEqual(self.coven("refresh", "lib", "--registry", "registry", "--key", bob).returncode, 0)
        machine("op")
        self.snapshot(operator)
        self.assertEqual(self.snapshot_entries()["lib"][2], bob_print)
        self.assertIn(f"lib 1.0.0: owners {alice_print} {bob_print}", fetch("1.0.0", "out-b").stdout)
        self.assertIn("does not list this version", fetch("1.0.2", "out-c").stderr)
        # The rotations and the registry's copy of them removed: what clients are served is what
        # the snapshot bound, the operator restores the copy from its own, and bob's next update
        # is admitted (adversarial review of 9cb1718: the package was frozen for good).
        rotations.unlink()
        for kept in (self.root / "registry/lib/chains").iterdir():
            kept.unlink()
        machine("rescue")
        self.assertEqual(self.coven("revoke", "lib", "1.0.0", "--registry", "registry", "--reason", "stolen", "--key", bob).returncode, 0)
        machine("op")
        self.snapshot(operator)
        self.assertIn("revoked by its publisher: stolen", fetch("1.0.0", "out-d").stderr)
        # A writer puts back an older snapshot of the operator's own, from before the hand-off was
        # recorded: a machine without its own copy does not build on it unless the operator states
        # the latest snapshot, and then refuses the older one (adversarial review of 384ab92).
        latest = self.module().parse_snapshot((self.root / "registry/snapshot").read_bytes()).sequence
        (self.root / "registry/snapshot").write_bytes(first_snapshot)
        machine("op-3")
        self.assertIn("give --min-base", self.coven("snapshot", "--registry", "registry", "--key", operator, "--id", "test").stderr)
        self.assertIn(f"older than snapshot {latest}", self.coven("snapshot", "--registry", "registry", "--key", operator, "--id", "test",
                                                                  "--min-base", str(latest)).stderr)

    def test_a_stale_operator_machine_builds_on_nothing_older_than_the_operator_states(self):
        """A writer puts back an old operator machine's own old snapshot, from before a hand-off it
        records, deletes the rotations and their copies, and plants an index of the stolen,
        handed-away key. That machine, given the operator's latest sequence (`--min-base`), refuses
        its own stale copy as a base: before, it signed above it, without the `handed` lines,
        reusing a sequence already signed, and admitted the stolen key (adversarial review of
        363f952). A served snapshot of this key's that differs from the machine's copy, or does not
        parse, is refused, and so is a --min-base below 1."""
        self.enterContext(policy_signatures(packages=True))
        import datetime
        (alice, alice_print), (rescue, rescue_print), (bob, bob_print) = self.key("alice"), self.key("rescue"), self.key("bob")
        operator, _ = self.operator("operator")
        machine = lambda who: os.environ.update(XDG_STATE_HOME=str(self.root / f"state-{who}"))
        machine("alice")
        self.start_with_alices_release(alice, rescue)
        alice_copy = next((self.root / "state-alice/coven/signed").iterdir())
        machine("op-old")
        self.snapshot(operator)
        stale = (self.root / "registry/snapshot").read_bytes()
        machine("rescue")
        rotated = self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", bob, "--from-owner", alice_print,
                             "--from-index", alice_copy, "--min-sequence", "1")
        self.assertEqual(rotated.returncode, 0, rotated.stderr)
        machine("op")
        self.snapshot(operator)
        latest = self.module().parse_snapshot((self.root / "registry/snapshot").read_bytes())
        self.assertEqual((latest.entries["lib"][2], ("lib", alice_print) in latest.handed), (bob_print, True))
        (self.root / "registry/snapshot").write_bytes(stale)
        (self.root / "registry/lib/rotations").unlink()
        for kept in (self.root / "registry/lib/chains").iterdir():
            kept.unlink()
        self.signed_index(alice, 50, datetime.date.today() + datetime.timedelta(days=30), rescue_print, self.read_entries(), self.root / "registry/lib/index")
        machine("op-old")
        snapshot = lambda *more: self.coven("snapshot", "--registry", "registry", "--key", operator, "--id", "test", *more)
        # Without --min-base it signs nothing: its copy is indistinguishable from the latest (review
        # of the fix: run without it, the machine signed snapshot 2 again and admitted alice).
        self.assertIn("this machine's copy): give --min-base N", snapshot().stderr)
        self.assertIn(f"older than snapshot {latest.sequence}, which this operator states it signed", snapshot("--min-base", str(latest.sequence)).stderr)
        self.assertEqual((self.root / "registry/snapshot").read_bytes(), stale)
        for wrong in ("0", "-3"):
            self.assertIn("1 or above", snapshot("--min-base", wrong).stderr)
        # On the operator's own machine: an older snapshot of its key served, another one at its
        # copy's sequence, or one that does not parse, is refused, never signed over.
        machine("op")
        self.assertIn(f"older than this machine's copy {latest.sequence}", snapshot("--min-base", str(latest.sequence)).stderr)
        later = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0) + datetime.timedelta(hours=1)
        (self.root / "registry/snapshot").write_bytes(self.signed_snapshot(operator, latest.sequence, later, latest.entries))
        self.assertIn(f"another than this machine's copy {latest.sequence}", snapshot("--min-base", str(latest.sequence)).stderr)
        (self.root / "registry/snapshot").write_bytes(b"not a snapshot")
        self.assertIn("the registry's snapshot is malformed", snapshot("--min-base", str(latest.sequence)).stderr)
        # The snapshot deleted, on a new machine: no snapshot is not a first one (review of 2a41f8b:
        # it signed snapshot 1 again, without the hand-offs, and admitted alice).
        (self.root / "registry/snapshot").unlink()
        machine("op-new")
        self.assertIn("give --first", snapshot().stderr)
        self.assertFalse((self.root / "registry/snapshot").exists())

    def test_a_rotation_after_the_rotations_were_deleted_is_admitted(self):
        """A writer deletes `NAME/rotations` after alice->bob was bound. The recovery key hands lib to
        carol: the rotation builds on the bound chain the registry keeps, not the file served. Then
        to dave, and a writer cuts the file to `[bob->carol, carol->dave]`: the operator admits a
        served chain that repeats the end of the bound one after that overlap. Before, the
        rotation to dave was refused and the package froze (adversarial review of 363f952)."""
        self.enterContext(policy_signatures(packages=True))
        keys = {who: self.key(who) for who in ["alice", "rescue", "bob", "carol", "dave"]}
        (alice, alice_print), (rescue, _) = keys["alice"], keys["rescue"]
        operator, _ = self.operator("operator")
        machine = lambda who: os.environ.update(XDG_STATE_HOME=str(self.root / f"state-{who}"))
        machine("alice")
        self.start_with_alices_release(alice, rescue)
        alice_copy = next((self.root / "state-alice/coven/signed").iterdir())
        machine("op")
        self.snapshot(operator)
        rotations = self.root / "registry/lib/rotations"
        coven = self.module()
        for previous, new, deleted in [("alice", "bob", False), ("bob", "carol", True), ("carol", "dave", False)]:
            if deleted:
                rotations.unlink()
            machine("rescue")
            sequence = str(self.snapshot_entries()["lib"][0])
            rotated = self.coven("rotate", "lib", "--registry", "registry", "--key", rescue, "--new-key", keys[new][0], "--from-owner", keys[previous][1],
                                 "--min-sequence", sequence, *(["--from-index", alice_copy] if previous == "alice" else []))
            self.assertEqual(rotated.returncode, 0, rotated.stderr)
            # What the rotation wrote extends the chain the snapshot bound.
            self.assertTrue(coven.extends_bound(rotations.read_bytes(), self.snapshot_entries()["lib"][4]))
            if new == "dave":
                rotations.write_bytes(b"".join(coven.rotation_pieces(rotations.read_bytes())[1:]))
            machine("op")
            self.snapshot(operator)
            self.assertEqual(self.snapshot_entries()["lib"][2], keys[new][1])
        bound = coven.snapshot_chain(self.root / "registry/lib", "lib", coven.parse_snapshot((self.root / "registry/snapshot").read_bytes()))
        self.assertEqual(len(coven.rotation_pieces(bound)), 3)

    def test_without_a_recovery_key_a_stolen_earlier_key_does_not_freeze_rotation(self):
        """lib has no recovery key; alice -> bob is bound. A thief with alice's old key plants a
        longer chain to bob through its own key, `[alice->mallory, mallory->bob]`. Before, bob's
        rotation built on the longest chain reaching bob, the thief's, which the operator never
        admits (it does not extend the bound one): lib froze at bob. Now the owner's rotation builds
        only on the chain the pinned operator's snapshot binds, and says what it refused."""
        self.enterContext(policy_signatures(packages=True))
        keys = {who: self.key(who) for who in ["alice", "bob", "carol", "dave", "mallory"]}
        operator, operator_print = self.operator("operator")
        machine = lambda who: os.environ.update(XDG_STATE_HOME=str(self.root / f"state-{who}"))
        rotate = lambda who, new, *pins: self.coven("rotate", "lib", "--registry", "registry", "--key", keys[who][0], "--new-key", keys[new][0], *pins)
        machine("alice")
        self.start_with_alices_release(keys["alice"][0])
        machine("op")
        self.snapshot(operator)
        stale = (self.root / "registry/snapshot").read_bytes()  # binds no rotations for lib
        # A writer's junk where no rotations are bound blocks nothing (review of 65cab4f).
        (self.root / "registry/lib/rotations").write_bytes(b"junk\n")
        machine("alice")
        pinned = ("--operator", operator_print, "--registry-id", "test")
        self.assertEqual(rotate("alice", "bob", *pinned, "--min-snapshot", "1").returncode, 0)
        machine("op")
        self.snapshot(operator)
        coven = self.module()
        public = {who: self.public(key) for who, (key, _) in keys.items()}
        planted = b"".join(self.sign_object("rotation", keys[old][0], coven.rotation_message("lib", keys[old][1], public[new], public[old]))
                           for old, new in [("alice", "mallory"), ("mallory", "bob")])
        (self.root / "registry/lib/rotations").write_bytes(planted)
        machine("bob")
        # The operator is pinned, or skipping the snapshot is explicit; without it, two chains reach
        # bob and neither extends the other: refused, nothing written.
        self.assertIn("give --operator", rotate("bob", "carol").stderr)
        self.assertIn("two different chains of rotations reach", rotate("bob", "carol", "--no-snapshot").stderr)
        self.assertIn("give --min-snapshot", rotate("bob", "carol", *pinned).stderr)
        # A writer serves snapshot 1, which binds nothing for lib, and deletes the bound copies, so
        # that only the thief's chain reaches bob: refused, as is a snapshot older than the one stated
        # (review of 208294e: built on the thief's chain, lib froze again).
        snapshot, chains = (self.root / "registry/snapshot").read_bytes(), self.root / "registry/lib/chains"
        shutil.copytree(chains, self.root / "chains-kept")
        shutil.rmtree(chains)
        (self.root / "registry/snapshot").write_bytes(stale)
        self.assertIn(f"snapshot 1 records its owner {keys['alice'][1]}, not this key", rotate("bob", "carol", *pinned, "--min-snapshot", "1").stderr)
        self.assertIn("older than snapshot 2", rotate("bob", "carol", *pinned, "--min-snapshot", "2").stderr)
        (self.root / "registry/snapshot").write_bytes(snapshot)
        self.assertIn("no rotations the registry keeps extend the ones snapshot 2 binds", rotate("bob", "carol", *pinned, "--min-snapshot", "2").stderr)
        self.assertEqual((self.root / "registry/lib/rotations").read_bytes(), planted)
        shutil.copytree(self.root / "chains-kept", chains)
        rotated = rotate("bob", "carol", *pinned, "--min-snapshot", "2")
        self.assertEqual(rotated.returncode, 0, rotated.stderr)
        self.assertIn("lib/rotations as served: they do not extend the rotations snapshot 2 binds", rotated.stderr)
        self.assertTrue(coven.extends_bound((self.root / "registry/lib/rotations").read_bytes(), self.snapshot_entries()["lib"][4]))
        machine("op")
        self.snapshot(operator)
        self.assertEqual(self.snapshot_entries()["lib"][2], keys["carol"][1])
        # bob's key stolen too: the writer serves snapshot 2 (owner bob, `[alice->bob]`), plants
        # `[alice->bob, bob->mallory, mallory->carol]` and deletes snapshot 3's chain. A stale
        # --min-snapshot 2 is refused: it records bob, not carol (adversarial review of 65cab4f).
        latest = self.snapshot_entries()["lib"][4]
        honest = coven.rotation_pieces((self.root / "registry/lib/rotations").read_bytes())
        (self.root / "registry/lib/rotations").write_bytes(honest[0] + b"".join(
            self.sign_object("rotation", keys[old][0], coven.rotation_message("lib", keys[old][1], public[new], public[old]))
            for old, new in [("bob", "mallory"), ("mallory", "carol")]))
        (chains / latest.removeprefix("sha384:")).unlink()
        (self.root / "registry/snapshot").write_bytes(snapshot)
        machine("carol")
        self.assertIn(f"snapshot 2 records its owner {keys['bob'][1]}, not this key", rotate("carol", "dave", *pinned, "--min-snapshot", "2").stderr)

    def test_an_owners_rotation_takes_no_snapshot_older_than_its_machine_verified(self):
        """The machine verifies snapshot 3 (rotating `other`); a writer then serves snapshot 2, which
        still records alice as lib's owner. With --min-snapshot 1, rotate built on it before; now
        the machine's record refuses it, and fetch takes the same floor. A new machine, without a
        record (or with one kept under another operator key), falls back to --min-snapshot."""
        self.enterContext(policy_signatures(packages=True))
        keys = {who: self.key(who) for who in ["alice", "bob"]}
        operator, operator_print = self.operator("operator")
        machine = lambda who: os.environ.update(XDG_STATE_HOME=str(self.root / f"state-{who}"))
        rotate = lambda name: self.coven("rotate", name, "--registry", "registry", "--key", keys["alice"][0], "--new-key", keys["bob"][0],
                                         "--operator", operator_print, "--registry-id", "test", "--min-snapshot", "1")
        machine("alice")
        self.start_with_alices_release(keys["alice"][0])
        self.published("b", "--key", keys["alice"][0], name="other")
        machine("op")
        self.snapshot(operator)
        self.snapshot(operator)
        second = (self.root / "registry/snapshot").read_bytes()
        self.snapshot(operator)
        third = (self.root / "registry/snapshot").read_bytes()
        machine("alice")
        self.assertEqual(rotate("other").returncode, 0)
        (self.root / "registry/snapshot").write_bytes(second)
        self.assertIn("serves snapshot 2, older than snapshot 3 this machine verified", rotate("lib").stderr)
        fetch = lambda into, *more: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", into, "--operator", operator_print, *more)
        self.assertIn("serves snapshot 2, older than snapshot 3", fetch("out-a").stderr)
        # A client's fetch records what it verified, a snapshot handed to it (--snapshot) included.
        machine("client")
        (self.root / "third").write_bytes(third)
        (self.root / "second").write_bytes(second)
        self.assertEqual(fetch("out-b", "--snapshot", self.root / "third").returncode, 0)
        self.assertIn("serves snapshot 2, older than snapshot 3 this machine verified", fetch("out-c", "--snapshot", self.root / "second").stderr)
        machine("new")
        self.module().record_freshness("test", keys["bob"][1], 9, [])  # under another key: does not apply
        rotated = rotate("lib")
        self.assertEqual(rotated.returncode, 0, rotated.stderr)

    def test_a_machine_that_saw_the_operator_rotate_refuses_the_retired_key(self):
        """Pinned to a retired operator key, a writer serving `operator-rotations` cut short let that
        key, stolen, sign a snapshot rotate trusted. A machine that saw the hand-over refuses the
        retired pin and a chain that does not extend the one it saw."""
        self.enterContext(policy_signatures(packages=True))
        import datetime
        keys = {who: self.key(who) for who in ["alice", "bob"]}
        (old, old_print), (new, new_print) = self.operator("op-old"), self.operator("op-new")
        rotate = lambda name, pin: self.coven("rotate", name, "--registry", "registry", "--key", keys["alice"][0], "--new-key", keys["bob"][0],
                                              "--operator", pin, "--registry-id", "test", "--min-snapshot", "1")
        self.start_with_alices_release(keys["alice"][0])
        self.published("b", "--key", keys["alice"][0], name="other")
        self.snapshot(old)
        self.assertEqual(self.rotate_operator(old, new).returncode, 0)
        # `operator-rotations` is read once: swapped for none after a first read, the chain checked
        # is the chain recorded (adversarial review of 88c38ae: the record forgot the hand-over).
        coven, reads = self.module(), []
        full = coven.operator_chain
        coven.operator_chain = lambda *args: reads.append(1) or (full(*args) if len(reads) == 1 else ([], []))
        self.assertEqual(coven.fresh_snapshot((self.root / "registry").resolve(), new_print, "test", 1)[2][3], [old_print, new_print])
        # Verified pinned to the new key: the old key, a link of the chain seen, is recorded too.
        rotated = rotate("other", new_print)
        self.assertEqual(rotated.returncode, 0, rotated.stderr)
        # The writer drops the rotations; the stolen old key signs a later snapshot.
        later = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
        (self.root / "registry/operator-rotations").unlink()
        (self.root / "registry/snapshot").write_bytes(self.signed_snapshot(old, 9, later, self.snapshot_entries()))
        self.assertIn(f"pin the current operator key, --operator {new_print}", rotate("lib", old_print).stderr)
        fetch = lambda pin: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out", "--operator", pin)
        self.assertIn(f"pin the current operator key, --operator {new_print}", fetch(old_print).stderr)
        coven = self.module()
        base, name = coven.freshness_state("test", new_print)
        self.assertEqual(coven.read_freshness("test", new_print), (2, [old_print, new_print]))
        # Pinned to the new key with the old key's record taken away: the chain cut short is refused.
        (base / coven.freshness_state("test", old_print)[1]).unlink()
        (self.root / "registry/snapshot").write_bytes(self.signed_snapshot(new, 9, later, self.snapshot_entries()))
        self.assertIn("do not extend the chain this machine saw", rotate("lib", new_print).stderr)
        self.assertIn("do not extend the chain this machine saw", fetch(new_print).stderr)


    def test_a_successor_never_builds_on_its_predecessors_later_snapshot(self):
        """The rotated-away key, stolen, signs a later snapshot (a revocation undone). The new key
        builds on its own copy, and on a new machine refuses it: a predecessor's snapshot counts
        only below the hand-over's sequence (automated review of c00d6a6)."""
        self.enterContext(policy_signatures(packages=True))
        import datetime
        (alice, _), (old, _), (new, _) = self.key("alice"), self.operator("op-old"), self.operator("op-new")
        self.start_with_alices_release(alice)
        self.snapshot(old)
        before_revocation = self.snapshot_entries()
        self.assertEqual(self.coven("revoke", "lib", "1.0.0", "--registry", "registry", "--reason", "CVE-7", "--key", alice).returncode, 0)
        self.snapshot(old)
        self.assertEqual(self.rotate_operator(old, new).returncode, 0)
        later = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
        stolen = self.signed_snapshot(old, 50, later, before_revocation)
        (self.root / "registry/snapshot").write_bytes(stolen)
        signed = self.snapshot(new)
        self.assertIn("snapshot 4 of registry test", signed.stdout, signed.stderr)
        self.assertEqual(self.snapshot_entries()["lib"][0], 2)
        os.environ["XDG_STATE_HOME"] = str(self.root / "new-machine")
        (self.root / "registry/snapshot").write_bytes(stolen)
        refused = self.coven("snapshot", "--registry", "registry", "--key", new, "--id", "test")
        self.assertIn("which handed the operator role to this key at snapshot 3", refused.stderr)
        # Below the hand-over too (adversarial review): only a snapshot this key signed is a base.
        (self.root / "registry/snapshot").write_bytes(self.signed_snapshot(old, 1, later, before_revocation))
        self.assertIn("which handed the operator role to this key at snapshot 3", self.coven("snapshot", "--registry", "registry", "--key", new, "--id", "test").stderr)

    def test_after_a_rotation_no_operator_key_starts_a_fresh_snapshot(self):
        """The snapshot deleted, on machines without copies: neither the current operator key nor
        the rotated-away one starts a new base, whatever --id says (automated review of 191617e)."""
        self.enterContext(policy_signatures(packages=True))
        (alice, _), (old, _), (new, _) = self.key("alice"), self.operator("op-old"), self.operator("op-new")
        self.start_with_alices_release(alice)
        self.snapshot(old)
        self.assertEqual(self.rotate_operator(old, new).returncode, 0)
        (self.root / "registry/snapshot").unlink()
        os.environ["XDG_STATE_HOME"] = str(self.root / "new-machine")
        for key in [new, old]:
            refused = self.coven("snapshot", "--registry", "registry", "--key", key, "--id", "test")
            self.assertIn("operator rotations and no snapshot this key signed", refused.stderr)
            self.assertFalse((self.root / "registry/snapshot").exists())

    def test_a_new_operator_machine_takes_the_registry_identifier_from_the_operator(self):
        """On a machine without its copy, the operator's own snapshot for another registry, replayed
        here, does not set this registry's identifier (automated review of 56ecbf7)."""
        self.enterContext(policy_signatures(packages=True))
        (alice, _), (operator, _) = self.key("alice"), self.operator("operator")
        self.start_with_snapshot(alice, operator)
        (self.root / "other").mkdir()
        shutil.copytree(self.root / "registry/lib", self.root / "other/lib")
        self.snapshot(operator, registry="other", registry_id="other")
        (self.root / "registry/snapshot").write_bytes((self.root / "other/snapshot").read_bytes())
        os.environ["XDG_STATE_HOME"] = str(self.root / "new-machine")
        self.assertIn("give --id", self.coven("snapshot", "--registry", "registry", "--key", operator).stderr)
        self.assertIn("serves a snapshot for registry other, not test", self.coven("snapshot", "--registry", "registry", "--key", operator, "--id", "test").stderr)
        self.assertIn("give --id", self.coven("rotate-operator", "--registry", "registry", "--key", operator, "--new-key", self.operator("next")[0]).stderr)

    def test_the_operator_writes_an_index_copy_only_inside_the_registry(self):
        """keep_index opens every directory from the registry root without following a link, and
        stages under a random name (automated review of 56ecbf7)."""
        coven = self.module()
        root = (self.root / "registry").resolve()
        (root / "lib").mkdir()
        from types import SimpleNamespace
        index = SimpleNamespace(sequence=1, raw=b"the verified index")
        # A staged name a writer could predict, planted first: the write still lands.
        (root / "lib/indexes").mkdir()
        (root / "lib/indexes" / f".index.{os.getpid()}.{index.sequence}").write_text("planted")
        coven.keep_index(root, "lib", index)
        self.assertEqual((root / "lib/indexes/1").read_bytes(), index.raw)
        # The package directory swapped for a link after the snapshot's checks: refused.
        victim = self.root / "victim"
        victim.mkdir()
        shutil.move(root / "lib", self.root / "lib-moved")
        (root / "lib").symlink_to(victim)
        with self.assertRaises(coven.CovenError):
            coven.keep_index(root, "lib", index)
        self.assertEqual(list(victim.iterdir()), [])

    def test_a_snapshot_or_rotation_for_another_registry_is_refused(self):
        """Replay: the same operator key runs two registries. What it signed for one is refused by
        a client of the other (automated review of c00d6a6)."""
        self.enterContext(policy_signatures(packages=True))
        (alice, _), (operator, operator_print), (successor, successor_print) = self.key("alice"), self.operator("operator"), self.operator("successor")
        self.start_with_snapshot(alice, operator)
        (self.root / "other").mkdir()
        shutil.copytree(self.root / "registry/lib", self.root / "other/lib")
        self.snapshot(operator, registry="other", registry_id="other")
        self.assertIn("identifier is test, set by its first snapshot", self.coven("snapshot", "--registry", "registry", "--key", operator, "--id", "other", "--min-base", str(self.latest_signed(operator))).stderr)
        fetch = lambda registry, into, pin=operator_print, *more: self.coven("fetch", "lib", "1.0.0", "--registry", registry, "--into", into, "--operator", pin, *more)
        ours = (self.root / "registry/snapshot").read_bytes()
        (self.root / "registry/snapshot").write_bytes((self.root / "other/snapshot").read_bytes())
        self.assertIn("signed for registry other, not test", fetch("registry", "out-a").stderr)
        # Nor does the operator sign over it: it restores its own (adversarial review of 363f952).
        self.assertIn("serves a snapshot for registry other, and this key's copy is of test",
                      self.coven("snapshot", "--registry", "registry", "--key", operator).stderr)
        (self.root / "registry/snapshot").write_bytes(ours)
        # A rotation made in the other registry, replayed into this one.
        self.assertEqual(self.rotate_operator(operator, successor, registry="other").returncode, 0)
        (self.root / "registry/operator-rotations").write_bytes((self.root / "other/operator-rotations").read_bytes())
        self.assertIn("signed for registry other, not test", fetch("registry", "out-b").stderr)
        self.assertEqual(fetch("other", "out-c", operator_print, "--registry-id", "other").returncode, 0)
        self.assertIn("--registry-id", self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-d", "--operator", operator_print, "--registry-id", "test", "--no-snapshot").stderr + "--registry-id")

    def test_only_the_operator_signs_a_snapshot(self):
        self.enterContext(policy_signatures(packages=True))
        (alice, _), (writer, writer_print), (operator, operator_print) = self.key("alice"), self.operator("writer"), self.operator("operator")
        self.start_with_snapshot(alice, operator)
        # Coven does not sign over another key's snapshot ...
        self.assertIn("give --id", self.coven("snapshot", "--registry", "registry", "--key", writer).stderr)
        self.assertIn("has not handed the operator role", self.coven("snapshot", "--registry", "registry", "--key", writer, "--id", "test").stderr)
        # ... and one a writer signs anyway is refused by a client pinned to the operator.
        import datetime
        later = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
        (self.root / "registry/snapshot").write_bytes(self.signed_snapshot(writer, 50, later, self.snapshot_entries()))
        refused = self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-a", "--operator", operator_print)
        self.assertIn(f"signed by {writer_print}, and its operator is {operator_print}", refused.stderr)
        self.assertFalse((self.root / "out-a").exists())
        # The operator's next snapshot repairs it, from its own copy.
        self.snapshot(operator)
        self.assertEqual(self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-b", "--operator", operator_print).returncode, 0)

    def test_the_operator_role_is_handed_on_by_its_key(self):
        self.enterContext(policy_signatures(packages=True))
        import datetime
        (alice, _), (old, old_print), (new, new_print), (writer, writer_print) = self.key("alice"), self.operator("op-old"), self.operator("op-new"), self.operator("writer")
        self.start_with_alices_release(alice)
        self.snapshot(old)
        rotated = self.rotate_operator(old, new)
        self.assertIn(f"operator {old_print} -> {new_print} (snapshot 2)", rotated.stdout, rotated.stderr)
        # A client pinned to the old key follows the role to the new one.
        seen = self.coven("verify-snapshot", "--registry", "registry", "--operator", old_print)
        self.assertIn(f"operators {old_print} {new_print}", seen.stdout, seen.stderr)
        self.assertEqual(self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-a", "--operator", old_print).returncode, 0)
        # The old key signs nothing more, even with its own copy.
        self.assertIn("handed the registry's operator role on", self.coven("snapshot", "--registry", "registry", "--key", old).stderr)
        self.assertIn("handed the registry's operator role on", self.coven("rotate-operator", "--registry", "registry", "--key", old, "--new-key", writer).stderr)
        self.assertIn("snapshot 3 of registry test", self.snapshot(new).stdout)
        # A stolen old key's snapshot is refused by a client that pins the new one, or the old one
        # while the rotation is there.
        rotations = self.root / "registry/operator-rotations"
        good, entries = rotations.read_bytes(), self.snapshot_entries()
        later = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
        (self.root / "registry/snapshot").write_bytes(self.signed_snapshot(old, 9, later, entries))
        fetch = lambda into, pin: self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", into, "--operator", pin)
        self.assertIn(f"its operator is {new_print}", fetch("out-b", old_print).stderr)
        rotations.unlink()
        self.assertIn(f"signed by {old_print}, and its operator is {new_print}", fetch("out-c", new_print).stderr)
        # A rotation rewritten by a writer, from the old key to its own: not signed by the old key.
        rotations.write_bytes(self.forge("operator-rotation", writer, self.module().operator_rotation_message("test", self.public(writer), 3, writer_print)))
        self.assertIn("each signed by the key it hands the role from", fetch("out-d", old_print).stderr)
        rotations.write_bytes(good)
        self.snapshot(new)
        self.assertEqual(fetch("out-e", old_print).returncode, 0)

    # The suite, witchy-2: ML-DSA-65 in the engine, the seed delivered on a pipe.

    def test_the_operator_signs_in_the_engine_and_a_client_verifies_with_its_key_id(self):
        """An honest snapshot: signed by sign.wy with the seed on a pipe (from a file, or from a
        descriptor coven was started with), verified by verify.wy against the pinned key ID. The
        key ID of NIST's ACVP ML-DSA-65 key-generation seed is the SHA-384 of the public key that
        an independent implementation (Go's crypto/mldsa) derived from it (the compiler's
        tests/fixtures/crypto/mldsa65-kat)."""
        alice, _ = self.key("alice")
        acvp = self.root / "acvp"
        acvp.write_bytes(bytes.fromhex("464756A985E5DF03739D95DD309C1ED9C5B04254CC294E7E7EB9B9365EE15117"))
        acvp.chmod(0o600)
        made = self.coven("key", "--key", acvp)
        self.assertEqual(made.stdout, "key witchy-2:73615fc5aaa407846b296677b2803e29c3f555db426c69103a3cb45d584730c531ba2d818786b2a9d09b13100e3174f1\n", made.stderr)
        operator, operator_id = self.operator("operator")
        self.start_with_alices_release(alice)
        with open(operator, "rb") as seed:
            signed = subprocess.run([COVEN, "snapshot", "--registry", "registry", "--key", "fd:7", "--public-key", f"{operator}.pub", "--id", "test", "--first"],
                                    cwd=self.root, capture_output=True, text=True, pass_fds=(os.dup2(seed.fileno(), 7),))
        self.assertIn("snapshot 1 of registry test", signed.stdout, signed.stderr)
        raw = (self.root / "registry/snapshot").read_bytes()
        public = Path(f"{operator}.pub").read_text().strip()
        self.assertTrue(raw.startswith(f"witchy/v2/snapshot\0coven-snapshot 4\nsuite witchy-2\noperator {public}\nregistry test\n".encode()))
        self.assertEqual(operator_id, "witchy-2:" + hashlib.sha384(self.public(operator)).hexdigest())
        # Cross-check both implementations on the bytes this invocation signed.
        import test_signatures
        if test_signatures.available():
            guest = self.module()
            message, sig = guest.split_signed(raw, "snapshot", ValueError("invalid snapshot"))
            public_bytes = self.public(operator)
            with test_signatures.policy_signatures():
                native = self.module()
                self.assertEqual(native.witchy_verify_all([("snapshot", public_bytes, message, sig),
                                                          ("snapshot", public_bytes, message + b"x", sig)]), [True, False])
                independent = native.witchy_sign("snapshot", native.signing_key(operator), message)
            self.assertEqual(guest.witchy_verify_all([("snapshot", public_bytes, message, independent),
                                                     ("snapshot", public_bytes, message + b"x", independent)]), [True, False])
        coven = self.module()
        message, signature = coven.split_signed(raw, "snapshot", ValueError("invalid snapshot"))
        with self.assertRaises(coven.CovenError) as cross:
            coven.witchy_run_signer("operator-rotation", coven.signing_key(operator), message)
        self.assertIn("not in the signer's domain", str(cross.exception))
        self.assertEqual(coven.witchy_verify_all([("snapshot", self.public(operator), message, signature),
                                                   ("operator-rotation", self.public(operator), message, signature)]), [True, False])
        seen = self.coven("verify-snapshot", "--registry", "registry", "--operator", operator_id)
        self.assertIn(f"operators {operator_id}\n", seen.stdout, seen.stderr)
        self.assertIn("lib 1.0.0: snapshot 1", self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out", "--operator", operator_id).stdout)
        self.enterContext(policy_signatures(packages=True))
        # Another key's ID, an SSH fingerprint and an ID of another suite pin nothing.
        _, other_id = self.operator("other")
        self.assertIn(f"signed by {operator_id}, and its operator is {other_id}",
                      self.coven("verify-snapshot", "--registry", "registry", "--operator", other_id).stderr)
        self.assertIn("is an SSH key fingerprint", self.coven("verify-snapshot", "--registry", "registry", "--operator", "SHA256:" + "B" * 43).stderr)
        self.assertIn("is a witchy-1 key ID", self.coven("verify-snapshot", "--registry", "registry", "--operator", "witchy-1:" + operator_id[9:73]).stderr)
        self.assertIn("an operator key ID, witchy-2:", self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-b", "--operator", "witchy-3:" + operator_id[9:]).stderr)
        # The same digits under another algorithm's name: a sha256-length ID is no witchy-2 ID.
        self.assertIn("an operator key ID, witchy-2:", self.coven("fetch", "lib", "1.0.0", "--registry", "registry", "--into", "out-b", "--operator", operator_id[:73]).stderr)

    def test_a_snapshot_changed_or_signed_otherwise_is_refused(self):
        """A changed digest, a signature of another key, a truncated signature, an object of
        another role, and another suite: each refused by a client pinned to the operator."""
        self.enterContext(policy_signatures(packages=True))
        import base64, datetime
        alice, _ = self.key("alice")
        (operator, operator_id), (writer, _) = self.operator("operator"), self.operator("writer")
        self.start_with_snapshot(alice, operator)
        coven, good = self.module(), (self.root / "registry/snapshot").read_bytes()
        verify = lambda: self.coven("verify-snapshot", "--registry", "registry", "--operator", operator_id)
        def refused(raw, why):
            (self.root / "registry/snapshot").write_bytes(raw)
            self.assertIn(why, verify().stderr)
        message, signature = coven.split_signed(good, "snapshot", ValueError())
        digest = coven.parse_snapshot(good).entries["lib"][1]
        refused(good.replace(digest.encode(), (digest[:-1] + ("0" if digest[-1] != "0" else "1")).encode()), "signature does not verify")
        # The writer's signature over the operator's own snapshot, naming the operator's key.
        refused(coven.signed_object(message, coven.witchy_sign("snapshot", coven.signing_key(writer), message)), "signature does not verify")
        refused(message + b"signature " + base64.b64encode(signature[:-1]) + b"\n", "malformed")
        refused(good[:-6] + b"\n", "malformed")
        # Another role: an operator rotation statement, validly signed, served as the snapshot;
        # and the engine refuses to sign a snapshot under the rotation's role.
        rotation = coven.operator_rotation_message("test", self.public(operator), 2, operator_id)
        refused(coven.signed_object(rotation, coven.witchy_sign("operator-rotation", coven.signing_key(operator), rotation)), "malformed")
        # Another suite: the same snapshot naming witchy-1, validly signed; one in witchy-1's domain
        # (as the operator signed before the witchy-2 profile); and one signed with ssh-keygen.
        other = message.replace(b"\nsuite witchy-2\n", b"\nsuite witchy-1\n")
        refused(coven.signed_object(other, coven.witchy_sign("snapshot", coven.signing_key(operator), other)), "malformed")
        refused(good.replace(b"witchy/v2/", b"witchy/v1/", 1), "signed with witchy-1 (ML-DSA-44), which no role uses any more")
        ssh = b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAp7 operator\n"
        refused(ssh + b"coven-snapshot 1\nregistry test\nsequence 9\nexpires 2099-01-01T00:00:00Z\n-----BEGIN SSH SIGNATURE-----\nx\n-----END SSH SIGNATURE-----\n",
                "signed with ssh-keygen")
        (self.root / "registry/snapshot").write_bytes(good)
        self.assertEqual(verify().returncode, 0)
        (self.root / "registry/operator-rotations").write_bytes(ssh + b"coven-operator-rotation 1\n")
        self.assertIn("rotations are signed with ssh-keygen", verify().stderr)

    def test_the_operator_seed_reaches_only_the_engine(self):
        """A canary seed, from a file and from a descriptor: in no argument or variable of the
        compiler or the engine, on no descriptor of the compiler, in no output, file Coven writes,
        or grant document. The engine gets it on a pipe of its own."""
        canary = b"canary-seed:7f3a9c1e5b2d4086ff01"
        self.assertEqual(len(canary), 32)
        seed = self.root / "operator"
        seed.write_bytes(canary)
        seed.chmod(0o600)
        seen = self.root / "seen"
        wrappers = {}
        for variable, executable in (("WITCHYC", "witchyc"), ("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")):
            real = tool(variable, executable)
            self.assertIsNotNone(real, f"cannot find {executable}")
            wrapper = self.root / f"wrapper-{variable.lower()}"
            wrapper.write_text(f"#!/bin/sh\n{{ echo \"argv: $*\"; env; ls /dev/fd; for f in \"$@\"; do case \"$f\" in *.toml) cat \"$f\";; esac; done; }} >> {shlex.quote(str(seen))}\n"
                               f'exec {shlex.quote(real)} "$@"\n')
            wrapper.chmod(0o755)
            wrappers[variable] = str(wrapper)
        env = dict(os.environ, **wrappers)
        run = lambda *args, **more: subprocess.run([COVEN, *map(str, args)], cwd=self.root, capture_output=True, env=env, **more)
        made = run("operator-key", "--key", seed)
        alice, _ = self.key("alice")
        self.start_with_alices_release(alice)
        outputs = [made, run("snapshot", "--registry", "registry", "--key", seed, "--id", "test", "--first")]
        with open(seed, "rb") as stream:
            outputs.append(run("snapshot", "--registry", "registry", "--key", "fd:9", "--public-key", f"{seed}.pub", "--min-base", "1", pass_fds=(os.dup2(stream.fileno(), 9),)))
        for output in outputs:
            self.assertEqual(output.returncode, 0, output.stderr)
        recorded = seen.read_bytes()
        self.assertIn(b"--secret-slot=key:sign/witchy-2/snapshot:fd:", recorded)
        self.assertIn(b"--secret-object=key:", recorded)
        for form in [canary, canary.hex().encode(), canary.hex().upper().encode(), __import__("base64").b64encode(canary)]:
            self.assertNotIn(form, recorded)
            for output in outputs:
                self.assertNotIn(form, output.stdout + output.stderr)
            for path in [*(self.root / "registry").rglob("*"), *(self.root / "state").rglob("*")]:
                if path.is_file():
                    self.assertNotIn(form, path.read_bytes())
        # The compiler's steps hold only the standard descriptors (and ls's own).
        for listing in recorded.split(b"argv: ")[1:]:
            if b"wasm-grants" in listing.split(b"\n")[0] or b" build " in b" " + listing.split(b"\n")[0]:
                self.assertNotIn(b"\n9\n", listing)
        # A seed of the wrong length, or one others can read, is refused before anything runs.
        # A seed given as --out by mistake is named, never quoted (review of the cutover).
        victim = self.root / "victim"
        victim.write_bytes(b"\x9f" + canary[1:])
        quoted = run("key", "--key", seed, "--out", victim)
        self.assertIn("holds something else", quoted.stderr.decode())
        self.assertNotIn(b"0x9f", quoted.stdout + quoted.stderr)
        self.assertEqual(victim.read_bytes(), b"\x9f" + canary[1:])
        coven = self.module()
        def refuses(source, message):
            with self.assertRaises(coven.CovenError) as error:
                coven.read_seed(str(source))
            self.assertIn(message, str(error.exception))
            self.assertNotIn(canary.decode(), str(error.exception))
        seed.write_bytes(canary[:31])
        before = seen.stat().st_size
        bad = run("snapshot", "--registry", "registry", "--key", seed, "--min-base", "2")
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn(b"not an ML-DSA-65 seed (32 bytes)", bad.stderr)
        self.assertNotIn(canary[:31], bad.stdout + bad.stderr)
        self.assertNotIn(b"--secret-slot=", seen.read_bytes()[before:])
        for invalid in (canary[:31], canary + b"x"):
            seed.write_bytes(invalid)
            refuses(seed, "not an ML-DSA-65 seed (32 bytes)")
        seed.write_bytes(canary)
        seed.chmod(0o640)
        refuses(seed, "readable by others")
        seed.chmod(0o600)
        if sys.platform == "darwin":
            subprocess.run(["chmod", "+a", "everyone allow read", seed], check=True)
            refuses(seed, "readable by others")
            subprocess.run(["chmod", "-N", seed], check=True)
        refuses("fd:11", "not a descriptor coven was started with")
        (self.root / "linked").symlink_to(seed)
        refuses(self.root / "linked", "a regular file, not a link")
        # The signer is given exactly the operator key's slot: a grant proposing any other secret
        # stops it before the seed is read.
        coven = self.module()
        proposed = coven.witchy_flags
        coven.witchy_flags = lambda *args: proposed(*args) + ["--secret-slot=other:seal:fd:4"]
        with self.assertRaises(coven.CovenError) as extra:
            coven.witchy_sign("snapshot", coven.signing_key(seed), coven.domain("snapshot"))
        self.assertIn("another secret than the signing key", str(extra.exception))

    def test_a_public_key_file_of_another_key_signs_nothing(self):
        """The decisions are made for the key the public key file names; the engine's key must be
        that one, or nothing is written."""
        alice, _ = self.key("alice")
        (operator, _), (other, other_id) = self.operator("operator"), self.operator("other")
        self.start_with_alices_release(alice)
        refused = self.coven("snapshot", "--registry", "registry", "--key", operator, "--public-key", f"{other}.pub", "--id", "test", "--first")
        self.assertIn(f"is witchy-2:", refused.stderr)
        self.assertIn(f"not {other_id}, which its public key file names", refused.stderr)
        self.assertFalse((self.root / "registry/snapshot").exists())
        # A rotation is signed whole before anything is written: a new key that is not its file's
        # leaves no hand-over behind.
        self.snapshot(operator)
        rotated = self.rotate_operator(operator, self.operator("next")[0], "--new-public-key", f"{other}.pub")
        self.assertIn("which its public key file names", rotated.stderr)
        self.assertFalse((self.root / "registry/operator-rotations").exists())


def make_initial_release(destination, *, release_only=False):
    """The publications the cases branch from (`start_with_alices_release`, `start_with_snapshot`):
    alice's release of lib, the operator's first snapshot of it, and the same release with the
    recovery key `rescue` (in `recovery/`). The engine-boundary cases use only
    Alice's initial release and create any snapshots they exercise themselves."""
    case = Registry()
    try:
        case.setUp()
        case.enterContext(policy_signatures(packages=True))
        alice, owner = case.key("alice")
        case.published("a", "--key", alice)
        registry = (case.root / "registry/lib").resolve()
        shutil.copytree(case.root / "a", destination / "a")
        shutil.copytree(registry, destination / "lib")
        shutil.copy2(case.module().signed_state(registry, "lib", owner), destination / "owner-index")
        if release_only:
            return
        operator, fingerprint = case.operator("operator")
        case.snapshot(operator)
        shutil.copy2(registry.parent / "snapshot", destination / "snapshot")
        shutil.copytree(registry / "indexes", destination / "indexes")
        shutil.copy2(case.module().operator_state(registry.parent, fingerprint), destination / "operator-copy")
        case.doCleanups()
        case.setUp()
        case.enterContext(policy_signatures(packages=True))
        alice, owner = case.key("alice")
        rescue, _ = case.key("rescue")
        case.published("a", "--key", alice, "--recovery-key", f"{rescue}.pub")
        registry = (case.root / "registry/lib").resolve()
        recovered = destination / "recovery"
        shutil.copytree(case.root / "a", recovered / "a")
        shutil.copytree(registry, recovered / "lib")
        shutil.copy2(case.module().signed_state(registry, "lib", owner), recovered / "owner-index")
    finally:
        case.doCleanups()

if __name__ == "__main__":
    unittest.main()
