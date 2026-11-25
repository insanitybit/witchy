"""A package of several files through Coven's commands: what `pack` and `publish` read and
refuse, what a served registry refuses on upload, and what a client refuses on fetch. The
launcher's decisions are called in this process; `publish.wy` and `fetch.wy` are represented as
in the other policy tests (`policy_signatures(packages=True)` checks the record and body
together), and the registry suite runs the real worker.
Run: python3 witchy2/test_release_registry.py"""
import contextlib
import hashlib
import io
import os
import shutil
import socket
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from cli_test_support import fresh_coven, invoke_coven, policy_signatures
from test_release_format import FILES, LIB, NAMES, ONE, TEXT, V2, V2_DIGEST, V2_RECORD, codec, pin, raw

HERE = Path(__file__).resolve().parent
COVEN = HERE / "coven"
V1_DIGEST = pin(b"witchy-package 1\nname lib\nversion 1.0.0\n\n" + ONE)


class ReleaseRegistry(unittest.TestCase):
    def setUp(self):
        self.enterContext(policy_signatures(packages=True))
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        (self.root / "registry").mkdir()
        previous = os.environ.get("XDG_STATE_HOME")
        os.environ["XDG_STATE_HOME"] = str(self.root / "state")
        self.addCleanup(lambda: os.environ.pop("XDG_STATE_HOME") if previous is None else os.environ.update(XDG_STATE_HOME=previous))
        self.package = self.root / "package"
        self.write(FILES)

    def write(self, files, version="1.1.0", source="lib.wy", requires=""):
        for path, data in files.items():
            (self.package / path).parent.mkdir(parents=True, exist_ok=True)
            (self.package / path).write_bytes(data)
        (self.package / "witchy.toml").write_text(f'[package]\nname = "lib"\nversion = "{version}"\nsource = "{source}"\n\n[dependencies]\n{requires}')

    def coven(self, *args):
        if args[0] == "fetch":
            args = (*args, "--no-snapshot")
        return invoke_coven([COVEN, *map(str, args)], self.root)

    def test_publication_manifest_is_bounded_and_requires_one_complete_toml_document(self):
        cli = fresh_coven(COVEN)
        manifest = self.package / "witchy.toml"
        original = manifest.read_bytes()
        self.assertEqual(cli.publication_source(self.package)[3], codec.pack("lib", "1.1.0", {}, "lib.wy", FILES))
        manifest.write_bytes(original + b"#" + b"x" * (131072 - len(original) - 1))
        self.assertEqual(cli.publication_source(self.package)[3], codec.pack("lib", "1.1.0", {}, "lib.wy", FILES))
        cases = (
            (manifest.read_bytes() + b"x", "at most 131072 bytes"),
            (original + b'[unused]\na = 1\na = 2\n', "invalid TOML 1.0"),
            (original + b'base = 2\n', "invalid version"),
            (original.replace(b'1.1.0"', b'1.1.0\\nend"'), "invalid version"),
            (original + b"# \xff", "must be UTF-8"),
            (original.replace(b'lib.wy"', b'../outside.wy"'), "inside the package directory"),
        )
        for wire, message in cases:
            with self.subTest(message=message):
                manifest.write_bytes(wire)
                with self.assertRaisesRegex(cli.CovenError, message):
                    cli.publication_source(self.package)
        manifest.unlink()
        os.mkfifo(manifest)
        with self.assertRaisesRegex(cli.CovenError, "cannot read package manifest"):
            cli.publication_source(self.package)

    def test_manifest_worker_checks_all_coordinates_before_success(self):
        from coven_manifest import BUILD, read_publication

        cli = fresh_coven(COVEN)
        manifest = self.package / "witchy.toml"
        original = manifest.read_bytes()
        module = cli.witchy_module("read_package_manifest", BUILD)
        host = cli.tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")
        module_args = (module, f"--module-pin={pin(module.read_bytes())}")

        def read(wire, expected):
            manifest.write_bytes(wire)
            replies = []

            def run(*args, **kwargs):
                reply = subprocess.run(*args, **kwargs)
                replies.append(reply.stdout)
                return reply

            if expected is None:
                self.assertEqual(read_publication(manifest, module_args, host, run, pin(wire)),
                                 ("lib", "1.1.0", "lib.wy", {"base": "2.0.0"}, True))
                self.assertTrue(replies[0].startswith(b"ok-v2\n"))
            else:
                with self.assertRaisesRegex(ValueError, expected):
                    read_publication(manifest, module_args, host, run, pin(wire))
                self.assertFalse(replies[0].startswith(b"ok"))

        read(original + b'base = "2.0.0"\n', None)
        for wire, message in (
            (original.replace(b'"lib"', b'"Lib"'), "invalid package name"),
            (original.replace(b'"1.1.0"', b'"01.1.0"'), "invalid version"),
            (original + b'base = "2.0.0"\nbad = 2\n', "invalid version"),
            (original + b'snapshot = "2.0.0"\n', "reserved for the registry"),
            (original + b'lib = "1.1.0"\n', "cannot depend on itself"),
        ):
            with self.subTest(message=message):
                read(wire, message)

    def test_manifest_pin_checks_raw_bytes_and_refuses_before_remote_publication(self):
        cli = fresh_coven(COVEN)
        manifest = self.package / "witchy.toml"
        original = manifest.read_bytes().replace(b"\n", b"\r\n")
        manifest.write_bytes(original)
        expected = pin(original)
        self.assertEqual(cli.publication_source(manifest, manifest_pin=expected)[3],
                         codec.pack("lib", "1.1.0", {}, "lib.wy", FILES))
        alias = self.root / "linked.toml"
        alias.symlink_to(manifest)
        with self.assertRaisesRegex(cli.CovenError, "requires the absolute, resolved manifest path"):
            cli.publication_source(alias, manifest_pin=expected)
        # Even two TOML documents with the same meaning are different publication inputs.
        manifest.write_bytes(original.replace(b"\r\n", b"\n"))
        with socket.create_server(("127.0.0.1", 0)) as listener:
            listener.setblocking(False)
            result = self.coven("publish", manifest, "--manifest-pin", expected,
                                "--registry", f"http://127.0.0.1:{listener.getsockname()[1]}")
            self.assertEqual((result.returncode, result.stdout), (1, ""))
            self.assertIn("package manifest differs from its expected SHA-384 pin", result.stderr)
            with self.assertRaises(BlockingIOError):
                listener.accept()

    def test_manifest_and_source_use_the_same_held_package_directory(self):
        cli = fresh_coven(COVEN)
        manifest = self.package / "witchy.toml"
        original = manifest.read_bytes()
        previous = self.root / "previous"
        replacement = self.root / "replacement"
        shutil.copytree(self.package, replacement)
        (replacement / "lib.wy").write_bytes(b"pub fn other() -> U64:\n    return 2\n")
        expected = codec.pack("lib", "1.1.0", {}, "lib.wy", FILES)

        reader = cli.shipped_module("coven_manifest.py", cli.MANIFEST_READER_DIGEST,
                                    "_coven_manifest_" + cli.MANIFEST_READER_DIGEST, "manifest reader")
        inspect = reader.read_publication
        pack = cli.packed_release

        def replace_directory():
            self.package.rename(previous)
            replacement.rename(self.package)

        def replace_during_review(*args, **kwargs):
            self.assertEqual(kwargs["input_path"].read_bytes(), original)
            reviewed = inspect(*args, **kwargs)
            replace_directory()
            return reviewed

        with patch.object(reader, "read_publication", replace_during_review):
            with self.assertRaisesRegex(cli.CovenError, "package directory changed"):
                cli.publication_source(manifest, manifest_pin=pin(original))
        self.package.rename(replacement)
        previous.rename(self.package)

        def replace_before_pack(*args, **kwargs):
            replace_directory()
            return pack(*args, **kwargs)

        with patch.object(cli, "packed_release", replace_before_pack):
            self.assertEqual(cli.publication_source(manifest, manifest_pin=pin(original))[3], expected)

    def test_a_package_of_several_files_is_packed_published_and_fetched_whole(self):
        base = self.root / "base"
        base.mkdir()
        (base / "base.wy").write_bytes(ONE)
        (base / "witchy.toml").write_text('[package]\nname = "base"\nversion = "2.0.0"\nsource = "base.wy"\n')
        self.assertEqual(self.coven("publish", base, "--registry", "registry").returncode, 0)
        self.write(FILES, requires='base = "2.0.0"\n')
        # Files nothing imports, and the package's tests, are not in the release.
        (self.package / "scratch.wy").write_bytes(b"pub fn spare() -> U64:\n    return 0\n")
        (self.package / "test.wy").write_bytes(b'import "lib.wy"\n')
        cli = fresh_coven(COVEN)
        self.assertEqual(cli.publication_source(self.package), ("lib", "1.1.0", {"base": "2.0.0"}, V2))
        published = self.coven("publish", self.package, "--registry", "registry")
        self.assertIn(f"lib 1.1.0: published {V2_DIGEST}", published.stdout, published.stderr)
        slot = self.root / "registry/lib/1.1.0"
        self.assertEqual(((slot / "package").read_bytes(), (slot / "source.wy").read_bytes(), (slot / "digest").read_text()),
                         (V2_RECORD, LIB + NAMES + TEXT, V2_DIGEST))
        fetched = self.coven("fetch", "lib", "1.1.0", "--registry", "registry", "--into", "cache", "--expect", V2_DIGEST)
        self.assertEqual(fetched.stdout, f"lib 1.1.0: verified {V2_DIGEST}\nlib 1.1.0: unsigned\n", fetched.stderr)
        release = codec.parse((self.root / "cache/package").read_bytes(), (self.root / "cache/source.wy").read_bytes(), "lib", "1.1.0")
        self.assertEqual((release.digest, dict(release.files)), (V2_DIGEST, FILES))
        self.assertEqual(cli.inspected(V2_DIGEST, V2_RECORD, V2),
                         ["lib 1.1.0", "requires base 2.0.0", "entry lib.wy", *V2_RECORD.decode().splitlines()[5:8], V2_DIGEST])
        with self.assertRaisesRegex(cli.CovenError, "changed while it was inspected"):
            cli.inspected(V2_DIGEST, V2_RECORD, V2 + b" ")
        with self.assertRaisesRegex(cli.CovenError, "does not have the digest"):
            tampered = V2.replace(b"return 64", b"return 65")
            cli.inspected(pin(tampered), V2_RECORD, tampered)
        # A package of one file is the release it always was, byte for byte.
        self.write({"lib.wy": ONE}, version="1.0.0")
        self.assertEqual(cli.publication_source(self.package)[3], b"witchy-package 1\nname lib\nversion 1.0.0\n\n" + ONE)
        self.assertIn(f"published {V1_DIGEST}", self.coven("publish", self.package, "--registry", "registry").stdout)
        self.assertEqual(cli.inspected(V1_DIGEST, cli.record("lib", "1.0.0", {}), cli.record("lib", "1.0.0", {}) + ONE), ["lib 1.0.0", V1_DIGEST])
        # A source in a directory of the package: that directory is the release's.
        (self.package / "src/util").mkdir(parents=True)
        for path, data in FILES.items():
            (self.package / "src" / path).write_bytes(data)
        self.write({}, source="src/lib.wy", requires='base = "2.0.0"\n')
        self.assertEqual(cli.publication_source(self.package)[3], V2)

    def test_pack_reads_no_link_and_nothing_outside_the_package(self):
        outside = self.root / "outside.wy"
        outside.write_bytes(b"pub fn secret() -> U64:\n    return 42\n")
        names = self.package / "names.wy"
        def refused(why):
            packed = self.coven("pack", self.package)
            self.assertEqual((packed.returncode, packed.stdout), (1, ""), packed.stderr)
            self.assertIn(why, packed.stderr)
            self.assertNotIn("secret", packed.stdout)
        names.unlink()
        names.symlink_to(outside)
        refused("is a symbolic link")
        names.unlink()
        os.link(outside, names)
        refused("more than one name")
        names.unlink()
        names.write_bytes(NAMES)
        (self.package / "lib.wy").write_bytes(b'import "../outside.wy"\n')
        refused("leaves the package's directory")
        (self.package / "lib.wy").write_bytes(b'import "Names.wy"\n')
        refused("lower case")
        # A test member is never in a release, as its entry or through an import.
        (self.package / "test.wy").write_bytes(b"")
        (self.package / "tests").mkdir()
        (self.package / "tests/helper.wy").write_bytes(ONE)
        for written in ("tests/helper.wy", "test.wy"):
            (self.package / "lib.wy").write_bytes(f'import "names.wy"\nimport "{written}"\n'.encode())
            refused("not a package test member")
        (self.package / "lib.wy").write_bytes(LIB)
        self.assertEqual(fresh_coven(COVEN).publication_source(self.package)[3], codec.pack("lib", "1.1.0", {}, "lib.wy", FILES))

    def test_a_served_registry_refuses_a_release_that_is_not_canonical(self):
        cli = fresh_coven(COVEN)
        tree = self.root / "tree"
        tree.mkdir()
        def upload(package, source, version="1.1.0"):
            fields = {"snapshot": b"s", "base": None, "index": b"i", "package": package, "source": source, "signature": b"g"}
            return cli.admit_publication(tree, ["packages", "lib", version], cli.http().encode(fields))
        body = LIB + NAMES + TEXT
        record = V2_RECORD.replace(b"requires base 2.0.0\n", b"")
        # Well formed: the next check is reached (there is no snapshot in this tree).
        with self.assertRaisesRegex(cli.CovenError, "operator snapshot changed"):
            upload(record, body)
        function = b"pub fn g() -> U64:\n    return 2\n"
        def parts(files, **options):
            return codec.split(raw(files, order=sorted(files), **options))
        refusals = {
            "another version": (record, body, "1.1.1"),
            "one byte of one file": (record, body.replace(b"return 64", b"return 65")),
            "bytes after the files": (record, body + b"\n"),
            "a file short": (record, body[:-1]),
            "files swapped": (record, NAMES + LIB + TEXT),
            "record bytes in the source": (record[:-1], b"\n" + body),
            "source bytes in the record": (record + LIB[:4], LIB[4:] + NAMES + TEXT),
            "a leading zero": (record.replace(b" 106 ", b" 0106 "), body),
            "files out of order": codec.split(raw({"names.wy": NAMES, "lib.wy": b'import "names.wy"\n'})),
            "a path that leaves": parts({"lib.wy": b'import "../x.wy"\n', "../x.wy": function}),
            "an absolute path": parts({"lib.wy": b'import "/x.wy"\n', "/x.wy": function}),
            "an upper-case path": parts({"lib.wy": b'import "X.wy"\n', "X.wy": function}),
            "a hidden path": parts({"lib.wy": b'import ".x.wy"\n', ".x.wy": function}),
            "a file that is no source": parts({"lib.wy": b'import "x.txt"\n', "x.txt": function}),
            "a file nothing imports": parts({"lib.wy": function, "x.wy": function}),
            "an import of a file not in it": parts({"lib.wy": b'import "x.wy"\nimport "y.wy"\n', "x.wy": function}),
            "a file and a directory of one name": parts({"lib.wy": b'import "a.wy"\nimport "a.wy/b.wy"\n', "a.wy": function, "a.wy/b.wy": function}),
            "one file in the second format": parts({"lib.wy": function}),
            "a package it does not require": parts({"lib.wy": b'import "x.wy"\nimport "@other"\n', "x.wy": function}),
            "an unknown format": (record.replace(b"witchy-package 2", b"witchy-package 3"), body),
            "the first format with a listing": (record.replace(b"witchy-package 2", b"witchy-package 1"), body),
            "not ASCII": (record.replace(b"names.wy", "nämes.wy".encode()), body),
        }
        for label, (package, source, *version) in refusals.items():
            with self.subTest(label), self.assertRaises(cli.CovenError) as refused:
                upload(package, source, *version)
            self.assertNotIn("operator snapshot changed", str(refused.exception), label)
        self.assertEqual(list(tree.iterdir()), [])
        # More than a version holds, in total.
        with self.assertRaisesRegex(cli.CovenError, "exceeds the supported size"):
            upload(record, body + b" " * (1 << 20))
        # The first format is accepted as before, and refused as before.
        first = b"witchy-package 1\nname lib\nversion 1.0.0\n\n"
        with self.assertRaisesRegex(cli.CovenError, "operator snapshot changed"):
            upload(first, V2, "1.0.0")
        with self.assertRaisesRegex(cli.CovenError, "self dependency"):
            upload(first.replace(b"\n\n", b"\nrequires lib 1.0.0\n\n"), ONE, "1.0.0")

    def test_a_client_refuses_a_release_with_a_tampered_file(self):
        published = self.coven("publish", self.package, "--registry", "registry")
        digest = published.stdout.split("published ")[1].split()[0]
        slot = self.root / "registry/lib/1.1.0"
        record, body = (slot / "package").read_bytes(), (slot / "source.wy").read_bytes()
        def refused(why, *expect):
            fetched = self.coven("fetch", "lib", "1.1.0", "--registry", "registry", "--into", "cache", *expect)
            self.assertEqual(fetched.returncode, 1, fetched.stdout)
            self.assertIn(why, fetched.stderr)
            self.assertNotIn("verified", fetched.stdout)
            self.assertEqual(list((self.root / "cache").iterdir()), [])
        def serve(package, source, recorded=None):
            (slot / "package").write_bytes(package)
            (slot / "source.wy").write_bytes(source)
            (slot / "digest").write_text(recorded or pin(package + source))
        tampered = body.replace(b"return 64", b"return 65")
        # One file changed: the slot no longer hashes to its record.
        serve(record, tampered, digest)
        refused("does not match its recorded digest")
        # The registry's record recomputed: the file is not the one its line lists; and with
        # the line rewritten too, it is another release than the one locked.
        serve(record, tampered)
        refused("the release's file 'names.wy' does not have the digest its record lists")
        rewritten = record.replace(pin(NAMES).encode(), pin(NAMES.replace(b"return 64", b"return 65")).encode())
        serve(rewritten, tampered)
        refused("not the locked digest", "--expect", digest)
        # Whatever a registry's writer recomputes, a release that is not canonical, or names a
        # path outside it, is never copied.
        function = b"pub fn g() -> U64:\n    return 2\n"
        for files, why in [({"lib.wy": b'import "../x.wy"\n', "../x.wy": function}, "`.` or `..`"),
                           ({"lib.wy": b'import "x.wy"\n', "x.wy": function, "y.wy": function}, "which nothing in it imports"),
                           ({"lib.wy": function}, "holds 2 to 64 files")]:
            serve(*codec.split(raw(files, order=sorted(files))))
            refused(why)
        serve(record, body + b"\n")
        refused("not exactly the files")
        serve(record[:-1], b"\n" + body)
        refused("not the record of lib 1.1.0")
        serve(record.replace(b"version 1.1.0", b"version 1.1.1"), body)
        refused("not the record of lib 1.1.0")
        serve(record, body)
        fetched = self.coven("fetch", "lib", "1.1.0", "--registry", "registry", "--into", "cache", "--expect", digest)
        self.assertEqual(fetched.returncode, 0, fetched.stderr)
        self.assertEqual(sorted(path.name for path in (self.root / "cache").iterdir()), ["package", "source.wy"])

    def test_a_slot_swapped_between_the_check_and_the_copy_keeps_nothing(self):
        """The worker refuses a release changed before its read when the replacement contains
        an unreachable file, even if it matches the recorded digest."""
        self.assertEqual(self.coven("publish", self.package, "--registry", "registry").returncode, 0)
        slot = self.root / "registry/lib/1.1.0"
        function = b"pub fn g() -> U64:\n    return 2\n"
        files = {"lib.wy": b'import "x.wy"\n', "x.wy": function, "hidden.wy": function}
        record, body = codec.split(raw(files, order=sorted(files)))
        (slot / "digest").write_text(pin(record + body))
        cli = fresh_coven(COVEN)
        real = cli.run_program
        def swapped(program, grants, temp):
            if program == "fetch":
                (slot / "package").write_bytes(record)
                (slot / "source.wy").write_bytes(body)
            return real(program, grants, temp)
        cli.run_program = swapped
        with contextlib.chdir(self.root), contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err:
            fetched = cli.fetch("registry", "lib", "1.1.0", "cache", None, no_snapshot=True)
        self.assertFalse(fetched)
        self.assertIn("which nothing in it imports", err.getvalue())
        self.assertNotIn("verified", out.getvalue())
        self.assertEqual(list((self.root / "cache").iterdir()), [])


    def key(self, name):
        seed = self.root / name
        seed.write_bytes((HERE / "test-keys" / name).read_bytes())
        seed.chmod(0o600)
        Path(f"{seed}.pub").write_text((HERE / "test-keys" / f"{name}.pub").read_text())
        return seed

    def test_a_served_registry_holds_a_release_of_several_files_to_its_owner_and_signed_record(self):
        """An upload of several files, signed, against a registry where alice owns `lib`: only
        her signed index and release signature for exactly the uploaded bytes at the uploaded
        coordinate are admitted."""
        alice, bob, operator = self.key("alice"), self.key("bob"), self.key("operator")
        self.write({"lib.wy": ONE}, version="1.0.0")
        first = self.coven("publish", self.package, "--registry", "registry", "--key", alice)
        self.assertEqual(first.returncode, 0, first.stderr)
        snap = self.coven("snapshot", "--registry", "registry", "--key", operator, "--first", "--id", "test")
        self.assertEqual(snap.returncode, 0, snap.stderr)
        cli = fresh_coven(COVEN)
        tree = self.root / "registry"
        body = LIB + NAMES + TEXT
        record = V2_RECORD.replace(b"requires base 2.0.0\n", b"")
        other_body = body.replace(b"return 64", b"return 65")
        other = codec.split(codec.pack("lib", "1.1.0", {}, "lib.wy", {**FILES, "names.wy": NAMES.replace(b"return 64", b"return 65")}))
        self.assertEqual(other[1], other_body)
        current = cli.read_index(tree / "lib", "lib")
        def upload(seed, signed=(record, body), sent=(record, body), route=("lib", "1.1.0"), signs=None, version="1.1.0"):
            """`seed` signs an index adding `signed` as `version`; `sent` is uploaded at `route`."""
            key = cli.signing_key(seed)
            digest = cli.digest_of(signed[0] + signed[1])
            scratch = Path(tempfile.mkdtemp(dir=self.root)) / "lib"
            scratch.mkdir()
            entries = dict(current.entries)
            entries[version] = (digest, "active", "")
            cli.write_index(scratch, "lib", key, current.sequence + 1, current.recovery, entries, cli.VALID_DAYS, keep=False)
            message = cli.release_message("lib", version, cli.digest_of(sent[0] + sent[1]) if signs == "sent" else digest, key.public)
            fields = {"snapshot": cli.digest_of((tree / cli.SNAPSHOT).read_bytes()).encode(), "base": cli.digest_of(current.raw).encode(),
                      "index": (scratch / cli.INDEX).read_bytes(), "package": sent[0], "source": sent[1],
                      "signature": cli.signed_object(message, cli.witchy_sign("release", key, message))}
            return cli.admit_publication(tree, ["packages", *route], cli.http().encode(fields))
        def untouched():
            # (The server runs this in a transaction that a refusal discards whole; called
            # directly, a refused slot may exist, and it holds nothing.)
            self.assertEqual([path.name for path in (tree / "lib").glob("1.1.0/*")], [])
            self.assertEqual((tree / "lib" / cli.INDEX).read_bytes(), current.raw)
        refusals = [
            ("another key signs the index and the release", dict(seed=bob), None),
            ("the owner's index names other bytes than those sent", dict(seed=alice, sent=other), "may only add the signed release"),
            ("the same, with the release signature over what was sent", dict(seed=alice, sent=other, signs="sent"), "may only add the signed release"),
            ("the record is of another version than the coordinate", dict(seed=alice, route=("lib", "1.2.0")), "1.2.0"),
            ("the record is of another name than the coordinate", dict(seed=alice, route=("other", "1.1.0")), "other"),
        ]
        for label, options, why in refusals:
            with self.subTest(label), self.assertRaises(cli.CovenError) as refused:
                upload(**options)
            if why:
                self.assertIn(why, str(refused.exception), label)
            untouched()
        # A release signature by the owner for another digest of the same coordinate.
        real_message = cli.release_message
        cli.release_message = lambda name, version, digest, public: real_message(name, version, cli.digest_of(other[0] + other[1]), public)
        try:
            with self.assertRaises(cli.CovenError):
                upload(alice)
        finally:
            cli.release_message = real_message
        untouched()
        # The owner's own, whole: admitted, and the slot holds exactly the release.
        self.assertEqual(upload(alice), {"digest": cli.digest_of(record + body).encode()})
        slot = tree / "lib/1.1.0"
        self.assertEqual(((slot / "package").read_bytes(), (slot / "source.wy").read_bytes()), (record, body))

    def test_publish_reads_no_link_and_no_special_file(self):
        """`publish`, as `pack`: nothing reaches the registry from a link or a FIFO, as the
        entry or as a file it imports."""
        outside = self.root / "outside.wy"
        outside.write_bytes(b"pub fn secret() -> U64:\n    return 42\n")
        def refused(why):
            published = self.coven("publish", self.package, "--registry", "registry")
            self.assertEqual(published.returncode, 1, published.stdout)
            self.assertRegex(published.stderr, why)
            self.assertEqual(list((self.root / "registry").iterdir()), [])
        for name in ("names.wy", "lib.wy"):
            path = self.package / name
            kept = path.read_bytes()
            path.unlink()
            path.symlink_to(outside)
            refused("is a symbolic link|must name a file inside the package directory")
            path.unlink()
            os.mkfifo(path)
            refused("not a regular file|no source file at")
            path.unlink()
            os.link(outside, path)
            refused("more than one name")
            path.unlink()
            path.write_bytes(kept)
        folder = self.package / next(path for path in FILES if "/" in path).split("/")[0]
        folder.rename(self.root / "folder-outside")
        folder.symlink_to(self.root / "folder-outside")
        refused("is a link or not a directory")


if __name__ == "__main__":
    unittest.main()
