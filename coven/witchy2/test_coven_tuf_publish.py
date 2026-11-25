"""Exercise the static mirror's publication boundary with stock signed TUF metadata."""
from datetime import datetime, timedelta, timezone
from contextlib import nullcontext
import fcntl
import importlib.util
import os
from pathlib import Path
import shutil
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from cli_test_support import fresh_coven
import coven_tuf
import coven_tuf_publish as mirror
from coven_https_client import DownloadRefused
from coven_tuf_refresh import ROOT_LIMIT, TIMESTAMP_LIMIT, install_root, refresh as refresh_held
from coven_tuf_select import select_target, selector_worker, write_cache
from coven_tuf_select import BUILD as SELECTOR_BUILD
from coven_approval import b64
from coven_staging import canonical, read_file
from coven_target import FORMAT
from witchy_release import digest_of


HOST = shutil.which(os.environ.get("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))
COMPILER = shutil.which(os.environ.get("WITCHYC", "witchyc"))


def refresh(run, cache, root, request, transport):
    with mirror.directory(cache.resolve()) as held:
        return refresh_held(run, held, root, request, transport)


class MirrorFetch(unittest.TestCase):
    def test_contended_rollback_state_times_out_without_replacing_the_checkpoint(self):
        origin = "https://example.test"
        pin = digest_of(b"initial root")
        with (tempfile.TemporaryDirectory() as directory,
              patch.dict(os.environ, {"XDG_STATE_HOME": directory}),
              patch.object(coven_tuf, "CACHE_LOCK_SECONDS", 0)):
            with coven_tuf.cache(origin, pin) as folder:
                checkpoint = "coven-fetch-history"
                write_cache(folder, checkpoint, b"accepted checkpoint")
                with self.assertRaisesRegex(ValueError, "TUF rollback state is busy"):
                    with coven_tuf.cache(origin, pin):
                        self.fail("entered the contended cache")
                with coven_tuf.cache("https://another.test", pin) as other:
                    self.assertFalse(os.path.samestat(os.fstat(folder), os.fstat(other)))
            with coven_tuf.cache(origin, pin) as reopened:
                self.assertEqual(read_file(checkpoint, 256, dir_fd=reopened), b"accepted checkpoint")

    def test_replaced_cache_directory_cannot_receive_checkpoint_under_the_old_lock(self):
        origin = "https://example.test"
        root = b"verified root fixture"
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {"XDG_STATE_HOME": directory}):
            with self.assertRaisesRegex(ValueError, "state directory changed during verification"):
                with coven_tuf.cache(origin, digest_of(root)) as held:
                    folder, = (Path(directory) / "coven" / "tuf").iterdir()
                    previous = folder.with_name("held-cache")
                    folder.rename(previous)
                    folder.mkdir(mode=0o700)
                    install_root(held, root, 1, root)
                    write_cache(held, "coven-fetch-history", b"verified checkpoint fixture")
            self.assertEqual(list(folder.iterdir()), [])
            self.assertEqual((previous / "root.json").readlink(), Path("root_history/1.root.json"))
            self.assertEqual((previous / "root_history/1.root.json").read_bytes(), root)
            self.assertEqual((previous / "coven-fetch-history").read_bytes(), b"verified checkpoint fixture")

    def test_bundled_workers_are_selected_and_pinned_by_the_toolchain(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            folder = root / "tuf-workers"
            folder.mkdir(mode=0o755)
            pins = {}
            for name in coven_tuf.TUF_WORKERS:
                body = name.encode()
                (folder / f"{name}.wasm").write_bytes(body)
                pins[name] = digest_of(body)
            manifest = folder / "pins.json"
            manifest.write_bytes(canonical(pins))
            manifest.chmod(0o644)
            options = SimpleNamespace(**{f"{role}_{field}": None for role in ("selector", "download", "extractor")
                                         for field in ("module", "pin")})
            with patch.object(coven_tuf, "__file__", str(root / "coven_tuf.py")):
                for url, program in (("https://example.test", "tuf_get"),
                                     ("http://127.0.0.1:8080", "tuf_get_loopback")):
                    workers = coven_tuf.mirror_workers(options, urlsplit(url), fetching=True)
                    self.assertEqual(workers, {
                        role: {"source": folder / f"{name}.wasm", "pin": pins[name]}
                        for role, name in (("download", program), ("selector", "tuf_fetch_select"),
                                           ("extractor", "tuf_extract"))
                    })
                folder.chmod(0o777)
                with self.assertRaisesRegex(ValueError, "invalid bundled TUF worker directory"):
                    coven_tuf.mirror_workers(options, urlsplit("https://example.test"), fetching=True)
                folder.chmod(0o755)
                manifest.unlink()
                with self.assertRaisesRegex(ValueError, "invalid bundled TUF worker pins"):
                    coven_tuf.mirror_workers(options, urlsplit("https://example.test"), fetching=True)

    def test_one_tls_origin_spelling_selects_the_rollback_state(self):
        for url in ("https://example.test", "https://example.test:8443", "http://127.0.0.1:8080"):
            self.assertEqual(coven_tuf.origin(url).geturl(), url)
        for url in ("https://example.test:443", "https://example.test:08443",
                    "https://EXAMPLE.test", "http://example.test:8080", "https://example.test/"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                coven_tuf.origin(url)

    def test_fetch_requires_the_selected_snapshot_before_downloading(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "release"
            snapshot = digest_of(b"selected snapshot")
            target = digest_of(b"selected target")
            package = b"verified package"
            args = SimpleNamespace(name="lib", version="1.0.0", registry="https://example.test",
                                   id="test", root_pin=digest_of(b"root"), trust_pin=digest_of(b"trust"),
                                   expect=digest_of(package), expect_snapshot=digest_of(b"other snapshot"),
                                   expect_target=target, out=output)
            downloaded = []
            def download(route, destination, *, length):
                downloaded.append((route, length))
                destination.write_bytes(b"target")

            def extract(work, request, trust):
                self.assertEqual((Path(work) / "target").read_bytes(), b"target")
                self.assertEqual(trust, b"trust")
                self.assertEqual(request["target"], target)
                self.assertEqual(request["source"], args.expect)
                return package, args.expect, "publisher", args.name, args.version

            client = SimpleNamespace(fetch=download, ca_pin=digest_of(b"CA bundle"))
            extractor = SimpleNamespace(extract=extract)
            route = "/targets/lib/package.coven"
            session = (None, client, extractor, b"root", b"trust", {})
            with (patch.object(coven_tuf, "mirror_session", side_effect=lambda *args: nullcontext(session)),
                  patch.object(coven_tuf, "cache", side_effect=lambda *args: nullcontext(Path(directory))),
                  patch.object(coven_tuf, "refresh", return_value=(snapshot, target, 6, route, 3)) as refresh):
                with self.assertRaisesRegex(ValueError, "verified TUF snapshot differs"):
                    coven_tuf.fetch(args, None, None)
                self.assertFalse(downloaded)
                self.assertFalse(output.exists())
                args.expect_snapshot = "invalid"
                with self.assertRaisesRegex(ValueError, "invalid expected TUF snapshot digest"):
                    coven_tuf.fetch(args, None, None)
                self.assertEqual(refresh.call_count, 1)
                args.expect_snapshot = snapshot
                args.expect_target = digest_of(b"other target")
                with self.assertRaisesRegex(ValueError, "verified TUF target differs"):
                    coven_tuf.fetch(args, None, None)
                self.assertFalse(downloaded)
                self.assertFalse(output.exists())
                args.expect_target = target
                self.assertEqual(coven_tuf.fetch(args, None, None),
                                 {"snapshot": snapshot, "snapshot_version": 3, "digest": args.expect,
                                  "publisher": "publisher", "target": target, "ca_pin": client.ca_pin})
                self.assertEqual(downloaded, [(route, 6)])
                self.assertEqual(output.read_bytes(), package)


@unittest.skipUnless(importlib.util.find_spec("tuf"), "install the signed fixture requirements")
@unittest.skipUnless(HOST and COMPILER, "install the Witchy compiler and Wasm host")
class MirrorPublication(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.build = staticmethod(fresh_coven(Path(__file__).with_name("coven")).build_module)

    def setUp(self):
        from cryptography.hazmat.primitives.asymmetric.mldsa import MLDSA87PrivateKey
        from securesystemslib.signer import CryptoSigner
        from tuf.api.metadata import Metadata, Root

        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        folder = Path(temp.name).resolve()
        incoming = folder / "incoming"
        self.metadata = incoming / "metadata"
        self.metadata.mkdir(parents=True)
        self.mirror = folder / "mirror"
        self.mirror.mkdir(mode=0o755)
        self.expiry = datetime.now(timezone.utc) + timedelta(days=1)
        self.signers = {role: CryptoSigner.generate_mldsa(scheme="ml-dsa-87/1")
                        for role in ("root", "targets", "snapshot", "timestamp", "lib")}
        second = self.second_root = CryptoSigner.generate_mldsa(scheme="ml-dsa-87/1")
        root = Root(expires=self.expiry)
        for role in ("root", "targets", "snapshot", "timestamp"):
            root.add_key(self.signers[role].public_key, role)
        root.add_key(second.public_key, "root")
        root.roles["root"].threshold = 2
        document = Metadata(root)
        document.sign(self.signers["root"])
        document.sign(second, append=True)
        root_file = folder / "root.json"
        root_file.write_bytes(document.to_bytes())
        trust_file = folder / "trust.json"
        witness = MLDSA87PrivateKey.generate().public_key().public_bytes_raw()
        trust_file.write_bytes(canonical({
            "format": FORMAT, "publishers": {}, "witness": b64(witness),
            "policy": {"registry": "test", "revision": 1, "origin": "https://release.example.test", "credentials": {}},
        }))
        self.args = SimpleNamespace(root=root_file, root_pin=digest_of(root_file.read_bytes()),
                                    trust=trust_file, trust_pin=digest_of(trust_file.read_bytes()),
                                    incoming=incoming, mirror=self.mirror)

    def publish(self):
        return mirror.publish(self.args, self.build, HOST)

    def assert_locked(self):
        with mirror.directory(self.mirror) as contender:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def issue(self, name, payload, filename):
        from tuf.api.metadata import Metadata, MetaFile

        document = Metadata(payload)
        document.sign(self.signers[name])
        data = document.to_bytes()
        (self.metadata / filename).write_bytes(data)
        return MetaFile.from_data(payload.version, data, ["sha384"])

    def update(self, version, *, delegated_version=None, timestamp_expiry=None):
        from tuf.api.metadata import DelegatedRole, Delegations, Snapshot, Targets, Timestamp

        if version == 1:
            key = self.signers["lib"].public_key
            roles = {"lib": DelegatedRole("lib", [key.keyid], 1, True, paths=["lib/*"])}
            self.top = self.issue("targets", Targets(expires=self.expiry, delegations=Delegations({key.keyid: key}, roles)),
                                  "1.targets.json")
        delegated_version = delegated_version or version
        if version == delegated_version:
            self.delegation = self.issue("lib", Targets(version=version, expires=self.expiry), f"{version}.lib.json")
        snapshot = self.issue("snapshot", Snapshot(version=version, expires=self.expiry, meta={
            "targets.json": self.top, "lib.json": self.delegation}), f"{version}.snapshot.json")
        self.issue("timestamp", Timestamp(version=version, expires=timestamp_expiry or self.expiry, snapshot_meta=snapshot),
                   "timestamp.json")

    def test_a_partial_write_keeps_the_old_timestamp_and_retry_finishes(self):
        self.update(1)
        self.publish()
        timestamp = self.mirror / mirror.TIMESTAMP
        previous = timestamp.read_bytes()
        self.update(2)
        module = self.mirror.parent / "prepare_repository.wasm"
        self.build("tuf_prepare_repository", module, mirror.BUILD)
        pin = digest_of(module.read_bytes())
        install = mirror.Publication.install
        prepare = mirror.prepare

        def check_prepare(*args):
            self.assert_locked()
            return prepare(*args)

        def interrupt(publication, path, source):
            if path == mirror.TIMESTAMP:
                self.assert_locked()
                raise OSError("simulated loss of publisher before commit")
            install(publication, path, source)

        def require_pinned_worker(*args):
            self.fail("pinned publication tried to build its worker")

        with patch.object(mirror, "prepare", check_prepare), patch.object(mirror.Publication, "install", interrupt):
            with self.assertRaisesRegex(OSError, "before commit"):
                mirror.publish(self.args, require_pinned_worker, HOST, source=module, pin=pin)
        self.assertEqual(timestamp.read_bytes(), previous)
        self.assertTrue((self.mirror / "metadata/2.snapshot.json").is_file())
        self.assertEqual(self.publish(), {"snapshot": 2, "timestamp": 2, "targets": 0})
        self.assertEqual(timestamp.read_bytes(), (self.args.incoming / mirror.TIMESTAMP).read_bytes())

    def test_changed_staged_worker_cannot_advance_the_mirror(self):
        self.update(1)
        self.publish()
        timestamp = self.mirror / mirror.TIMESTAMP
        previous = timestamp.read_bytes()
        self.update(2)
        module = self.mirror.parent / "prepare_repository.wasm"
        self.build("tuf_prepare_repository", module, mirror.BUILD)
        pin = digest_of(module.read_bytes())
        launch = mirror.run_with_lifeline

        def change_worker(run, command, **options):
            Path(command[1]).write_bytes(b"changed worker")
            return launch(run, command, **options)

        with patch.object(mirror, "run_with_lifeline", change_worker):
            with self.assertRaisesRegex(ValueError, "TUF publication module differs from its independent pin"):
                mirror.publish(self.args, self.build, HOST, source=module, pin=pin)
        self.assertEqual(timestamp.read_bytes(), previous)
        self.assertFalse((self.mirror / "metadata/2.snapshot.json").exists())
        self.assertEqual(mirror.publish(self.args, self.build, HOST, source=module, pin=pin)["snapshot"], 2)

    def test_invalid_generation_leaves_no_new_files(self):
        self.update(1)
        self.publish()
        previous = (self.mirror / mirror.TIMESTAMP).read_bytes()
        self.update(2, timestamp_expiry=datetime.now(timezone.utc) - timedelta(seconds=1))
        with self.assertRaisesRegex(ValueError, "invalid candidate generation"):
            self.publish()
        self.update(2)
        delegated = self.metadata / "2.lib.json"
        delegated.write_bytes((self.metadata / "1.lib.json").read_bytes())
        with self.assertRaisesRegex(ValueError, "invalid candidate generation"):
            self.publish()
        self.assertEqual((self.mirror / mirror.TIMESTAMP).read_bytes(), previous)
        self.assertFalse((self.mirror / "metadata/2.snapshot.json").exists())

    def test_an_immutable_object_keeps_its_original_bytes(self):
        original = Path("targets/lib") / ("a" * 96 + ".1.0.0.coven")
        (self.mirror / original.parent).mkdir(parents=True)
        installed = self.mirror / original
        installed.write_bytes(b"previously verified")
        source = self.args.root.parent / "staged.coven"
        source.write_bytes(installed.read_bytes())
        inode = installed.stat().st_ino
        with mirror.directory(self.mirror) as held:
            publication = mirror.Publication(held, os.fstat(held).st_dev)
            publication.install(original, source)
            self.assertEqual(installed.stat().st_ino, inode)
            source.write_bytes(b"different bytes")
            with self.assertRaisesRegex(ValueError, "immutable mirror object conflicts"):
                publication.install(original, source)
        self.assertEqual(installed.read_bytes(), b"previously verified")

    def test_fetch_selects_only_authenticated_metadata_after_root_rotation(self):
        from tuf.api.metadata import Metadata, Snapshot, TargetFile, Targets, Timestamp

        self.update(1)
        wire = b"sealed target selected by the metadata verifier"
        coordinate = "lib/1.0.0.coven"
        offer = TargetFile.from_data(coordinate, wire, ["sha384"])
        old_role = self.issue("lib", Targets(version=1, expires=self.expiry, targets={coordinate: offer}), "1.lib.json")
        old_snapshot = self.issue("snapshot", Snapshot(version=1, expires=self.expiry, meta={
            "targets.json": self.top, "lib.json": old_role}), "1.snapshot.json")
        self.issue("timestamp", Timestamp(version=1, expires=self.expiry, snapshot_meta=old_snapshot), "timestamp.json")
        old_timestamp = (self.metadata / "timestamp.json").read_bytes()
        delegated = self.issue("lib", Targets(version=2, expires=self.expiry, targets={coordinate: offer}), "2.lib.json")
        snapshot = self.issue("snapshot", Snapshot(version=5, expires=self.expiry, meta={
            "targets.json": self.top, "lib.json": delegated}), "5.snapshot.json")
        self.issue("timestamp", Timestamp(version=7, expires=self.expiry, snapshot_meta=snapshot), "timestamp.json")
        cache = self.args.root.parent / "cache"
        history = cache / "root_history"
        history.mkdir(parents=True)
        initial = self.args.root.read_bytes()
        root = Metadata.from_bytes(initial)
        root.signed.version = 2
        root.sign(self.signers["root"])
        root.sign(self.second_root, append=True)
        next_root = history / "2.root.json"
        next_root.write_bytes(root.to_bytes())
        (cache / "root.json").symlink_to("root_history/2.root.json")
        for name, source in (("targets.json", "1.targets.json"), ("lib.json", "2.lib.json"),
                             ("snapshot.json", "5.snapshot.json"), ("timestamp.json", "timestamp.json")):
            shutil.copyfile(self.metadata / source, cache / name)
        request = {"coordinate": coordinate, "registry": "test", "root": self.args.root_pin, "trust": self.args.trust_pin}

        def select():
            return select_target(self.build, HOST, cache, initial, self.args.trust.read_bytes(), request)

        snapshot_file = cache / "snapshot.json"
        snapshot_wire = snapshot_file.read_bytes()
        wrapped = digest_of(wire)
        route = f"/targets/lib/{wrapped.removeprefix('sha384:')}.1.0.0.coven"
        expected = (digest_of(snapshot_wire), wrapped, len(wire), route, 5)
        valid_root = next_root.read_bytes()
        root.sign(self.signers["root"])
        next_root.write_bytes(root.to_bytes())
        with self.assertRaisesRegex(ValueError, "metadata verification refused"):
            select()
        next_root.write_bytes(valid_root)
        self.assertEqual(select(), expected)
        checkpoint = cache / "coven-fetch-history"
        accepted = checkpoint.read_bytes()
        version_line, root_pin, old_format_body = accepted.split(b"\n", 2)
        self.assertEqual(root_pin, digest_of(valid_root).encode("ascii"))
        checkpoint.write_bytes(version_line + b"\n" + old_format_body)
        self.assertEqual(select(), expected)
        self.assertEqual(checkpoint.read_bytes(), accepted)
        timestamp = cache / "timestamp.json"
        latest_timestamp = timestamp.read_bytes()
        timestamp.write_bytes(old_timestamp)
        shutil.copyfile(self.metadata / "1.snapshot.json", snapshot_file)
        shutil.copyfile(self.metadata / "1.lib.json", cache / "lib.json")
        with self.assertRaisesRegex(ValueError, "metadata rollback refused"):
            select()
        self.assertEqual(checkpoint.read_bytes(), accepted)
        timestamp.write_bytes(latest_timestamp)
        shutil.copyfile(self.metadata / "2.lib.json", cache / "lib.json")
        snapshot_file.write_bytes(snapshot_wire + b"\n")
        with self.assertRaisesRegex(ValueError, "metadata verification refused"):
            select()
        snapshot_file.write_bytes(snapshot_wire)
        (cache / "root.json").unlink()
        (cache / "root.json").symlink_to("root_history/1.root.json")
        with self.assertRaisesRegex(ValueError, "metadata rollback refused"):
            select()
        (cache / "root.json").unlink()
        (cache / "root.json").symlink_to("root_history/2.root.json")
        next_root.write_bytes(valid_root + b"\n")
        with self.assertRaisesRegex(ValueError, "metadata rollback refused"):
            select()
        self.assertEqual(checkpoint.read_bytes(), accepted)
        next_root.write_bytes(valid_root)
        removed = self.issue("lib", Targets(version=3, expires=self.expiry), "3.lib.json")
        withdrawal = self.issue("snapshot", Snapshot(version=6, expires=self.expiry, meta={
            "targets.json": self.top, "lib.json": removed}), "6.snapshot.json")
        self.issue("timestamp", Timestamp(version=8, expires=self.expiry, snapshot_meta=withdrawal), "timestamp.json")
        for name, source in (("timestamp.json", "timestamp.json"), ("snapshot.json", "6.snapshot.json"),
                             ("lib.json", "3.lib.json")):
            shutil.copyfile(self.metadata / source, cache / name)
        self.assertIsNone(select())
        withdrawn = checkpoint.read_bytes()
        self.assertNotEqual(withdrawn, accepted)
        timestamp.write_bytes(latest_timestamp)
        snapshot_file.write_bytes(snapshot_wire)
        shutil.copyfile(self.metadata / "2.lib.json", cache / "lib.json")
        with self.assertRaisesRegex(ValueError, "metadata rollback refused"):
            select()
        self.assertEqual(checkpoint.read_bytes(), withdrawn)
        for name, source in (("timestamp.json", "timestamp.json"), ("snapshot.json", "6.snapshot.json"),
                             ("lib.json", "3.lib.json")):
            shutil.copyfile(self.metadata / source, cache / name)
        root.signed.version = 3
        root.sign(self.signers["root"])
        root.sign(self.second_root, append=True)
        third_root = root.to_bytes()
        (history / "3.root.json").write_bytes(third_root)
        (cache / "root.json").unlink()
        (cache / "root.json").symlink_to("root_history/3.root.json")
        next_root.write_bytes(valid_root + b"\n")
        with self.assertRaisesRegex(ValueError, "metadata rollback refused"):
            select()
        self.assertEqual(checkpoint.read_bytes(), withdrawn)
        next_root.write_bytes(valid_root)
        self.assertIsNone(select())
        self.assertTrue(checkpoint.read_bytes().startswith(f"3\n{digest_of(third_root)}\n".encode("ascii")))

    def test_refresh_requests_only_authenticated_metadata_versions(self):
        from tuf.api.metadata import Metadata

        self.update(1)
        initial = self.args.root.read_bytes()
        root = Metadata.from_bytes(initial)
        root.signed.version = 2
        root.sign(self.signers["root"])
        root.sign(self.second_root, append=True)
        (self.metadata / "2.root.json").write_bytes(root.to_bytes())
        cache = self.args.root.parent / "refresh"
        cache.mkdir()
        request = {"coordinate": "lib/1.0.0.coven", "registry": "test",
                   "root": self.args.root_pin, "trust": self.args.trust_pin}
        calls = []

        def fetch(route, destination, *, length=None, max_length=None):
            calls.append(route)
            name = route.removeprefix("/metadata/")
            if name.endswith(".root.json"):
                self.assertEqual(max_length, ROOT_LIMIT)
            elif name == "timestamp.json":
                self.assertEqual(max_length, TIMESTAMP_LIMIT)
            else:
                self.assertIsNone(max_length)
                self.assertIsNotNone(length)
            try:
                wire = (self.metadata / name).read_bytes()
            except FileNotFoundError:
                raise DownloadRefused(404) from None
            if length is not None:
                self.assertEqual(len(wire), length)
            destination.write_bytes(wire)

        transport = SimpleNamespace(fetch=fetch)
        select = self.enterContext(selector_worker(self.build, HOST, initial, self.args.trust.read_bytes(), request))

        def run():
            return refresh(select, cache, initial, request, transport)

        # An untrusted successor must not undo a rotation authenticated in this refresh.
        second_wire = (self.metadata / "2.root.json").read_bytes()
        root.signed.version = 3
        root.sign(self.signers["root"])
        invalid_root = self.metadata / "3.root.json"
        invalid_root.write_bytes(root.to_bytes())
        with self.assertRaisesRegex(ValueError, "metadata verification refused"):
            run()
        self.assertEqual(calls, ["/metadata/2.root.json", "/metadata/3.root.json"])
        self.assertEqual(os.readlink(cache / "root.json"), "root_history/2.root.json")
        self.assertEqual((cache / "root.json").read_bytes(), second_wire)
        self.assertFalse((cache / "root_history/3.root.json").exists())
        invalid_root.unlink()
        calls.clear()

        self.assertIsNone(run())
        self.assertEqual(calls, ["/metadata/3.root.json", "/metadata/timestamp.json",
                                 "/metadata/1.snapshot.json", "/metadata/1.targets.json", "/metadata/1.lib.json"])
        calls.clear()
        self.assertIsNone(run())
        self.assertEqual(calls, ["/metadata/3.root.json", "/metadata/timestamp.json"])
        downloaded_role = (self.metadata / "1.lib.json").read_bytes()
        cached_role = cache / "verified-lib.json"
        cached_role.write_bytes(b"x" + downloaded_role[1:])
        calls.clear()
        self.assertIsNone(run())
        self.assertEqual(calls, ["/metadata/3.root.json", "/metadata/timestamp.json", "/metadata/1.lib.json"])
        self.assertEqual(cached_role.read_bytes(), downloaded_role)
        self.update(2)
        calls.clear()
        self.assertIsNone(run())
        self.assertEqual(calls, ["/metadata/3.root.json", "/metadata/timestamp.json",
                                 "/metadata/2.snapshot.json", "/metadata/2.lib.json"])
        checkpoint = cache / "coven-fetch-history"
        previous = checkpoint.read_bytes()
        timestamp = self.metadata / "timestamp.json"
        changed = Metadata.from_bytes(timestamp.read_bytes())
        changed.sign(self.signers["targets"])
        timestamp.write_bytes(changed.to_bytes())
        calls.clear()
        with self.assertRaisesRegex(ValueError, "metadata verification refused"):
            run()
        self.assertEqual(calls, ["/metadata/3.root.json", "/metadata/timestamp.json"])
        self.assertEqual(checkpoint.read_bytes(), previous)
        fresh = self.args.root.parent / "refused-root"
        fresh.mkdir()
        root.signed.version = 2
        root.sign(self.signers["root"])
        (self.metadata / "2.root.json").write_bytes(root.to_bytes())
        calls.clear()
        with self.assertRaisesRegex(ValueError, "metadata verification refused"):
            refresh(select, fresh, initial, request, transport)
        self.assertEqual(calls, ["/metadata/2.root.json"])
        self.assertFalse((fresh / "root.json").exists())

    def test_interrupted_refresh_preserves_authenticated_metadata_floors(self):
        self.update(1)
        initial = self.args.root.read_bytes()
        request = {"coordinate": "lib/1.0.0.coven", "registry": "test",
                   "root": self.args.root_pin, "trust": self.args.trust_pin}
        cache = self.args.root.parent / "refresh"
        cache.mkdir()
        timestamp = self.metadata / "timestamp.json"
        first_timestamp = timestamp.read_bytes()
        first_delegation = self.delegation
        unavailable = set()
        calls = []

        def fetch(route, destination, *, length=None, max_length=None):
            name = route.removeprefix("/metadata/")
            calls.append(name)
            path = self.metadata / name
            if name in unavailable or not path.exists():
                raise DownloadRefused(404)
            destination.write_bytes(path.read_bytes())

        transport = SimpleNamespace(fetch=fetch)
        select = self.enterContext(selector_worker(self.build, HOST, initial, self.args.trust.read_bytes(), request))

        def run():
            return refresh(select, cache, initial, request, transport)

        self.assertIsNone(run())
        checkpoint = cache / "coven-fetch-history"
        first_floor = checkpoint.read_bytes()
        self.update(2)
        second_timestamp = timestamp.read_bytes()
        unavailable.add("2.snapshot.json")
        with self.assertRaises(DownloadRefused):
            run()
        self.assertEqual(checkpoint.read_bytes(), first_floor)
        timestamp.write_bytes(first_timestamp)
        calls.clear()
        with self.assertRaisesRegex(ValueError, "metadata rollback refused"):
            run()
        self.assertEqual(calls, ["2.root.json", "timestamp.json"])

        timestamp.write_bytes(second_timestamp)
        unavailable.clear()
        unavailable.add("2.lib.json")
        with self.assertRaises(DownloadRefused):
            run()
        self.assertTrue(checkpoint.read_bytes().endswith((self.metadata / "2.snapshot.json").read_bytes()))
        self.delegation = first_delegation
        self.update(3, delegated_version=1)
        calls.clear()
        with self.assertRaisesRegex(ValueError, "metadata rollback refused"):
            run()
        self.assertEqual(calls, ["2.root.json", "timestamp.json", "3.snapshot.json"])

    def test_refresh_recovers_after_authenticated_metadata_key_rotation(self):
        from securesystemslib.signer import CryptoSigner
        from tuf.api.metadata import Metadata

        self.update(1)
        initial = self.args.root.read_bytes()
        trust = self.args.trust.read_bytes()
        request = {"coordinate": "lib/1.0.0.coven", "registry": "test",
                   "root": self.args.root_pin, "trust": self.args.trust_pin}
        cache = self.args.root.parent / "recovery"
        cache.mkdir()
        root = Metadata.from_bytes(initial)
        timestamp = self.metadata / "timestamp.json"
        self.update(40)
        high_timestamp = timestamp.read_bytes()

        def fetch(route, destination, *, length=None, max_length=None):
            name = route.removeprefix("/metadata/")
            try:
                wire = (self.metadata / name).read_bytes()
            except FileNotFoundError:
                raise DownloadRefused(404) from None
            destination.write_bytes(wire)

        def sign_root(version):
            root.signed.version = version
            root.sign(self.signers["root"])
            root.sign(self.second_root, append=True)
            (self.metadata / f"{version}.root.json").write_bytes(root.to_bytes())

        select = self.enterContext(selector_worker(self.build, HOST, initial, trust, request))
        transport = SimpleNamespace(fetch=fetch)

        def run():
            return refresh(select, cache, initial, request, transport)

        self.assertIsNone(run())
        # Merely adding a timestamp key leaves the previous key sufficient.
        companion = CryptoSigner.generate_mldsa(scheme="ml-dsa-87/1")
        root.signed.add_key(companion.public_key, "timestamp")
        sign_root(2)
        self.update(2)
        with self.assertRaisesRegex(ValueError, "metadata rollback refused"):
            run()
        original = self.signers["timestamp"]
        replacement = CryptoSigner.generate_mldsa(scheme="ml-dsa-87/1")
        root.signed.revoke_key(original.public_key.keyid, "timestamp")
        root.signed.revoke_key(companion.public_key.keyid, "timestamp")
        root.signed.add_key(replacement.public_key, "timestamp")
        sign_root(3)
        self.signers["timestamp"] = replacement
        self.update(3)
        recovered_timestamp = timestamp.read_bytes()
        self.assertIsNone(run())
        self.assertIsNone(run())
        timestamp.write_bytes(high_timestamp)
        with self.assertRaisesRegex(ValueError, "metadata verification refused"):
            run()
        timestamp.write_bytes(recovered_timestamp)
        self.assertIsNone(run())

        self.update(30)
        self.assertIsNone(run())
        previous_snapshot = self.signers["snapshot"]
        replacement_snapshot = CryptoSigner.generate_mldsa(scheme="ml-dsa-87/1")
        root.signed.revoke_key(previous_snapshot.public_key.keyid, "snapshot")
        root.signed.add_key(replacement_snapshot.public_key, "snapshot")
        sign_root(4)
        self.signers["snapshot"] = replacement_snapshot
        self.update(4)
        self.assertIsNone(run())

    def test_catalog_requires_every_delegation_and_only_fetches_metadata(self):
        from securesystemslib.signer import CryptoSigner
        from tuf.api.metadata import DelegatedRole, Delegations, Snapshot, TargetFile, Targets, Timestamp

        self.signers["other"] = CryptoSigner.generate_mldsa(scheme="ml-dsa-87/1")
        keys = {role: self.signers[role].public_key for role in ("other", "lib")}
        parent = self.issue("targets", Targets(expires=self.expiry, delegations=Delegations(
            {key.keyid: key for key in keys.values()},
            {role: DelegatedRole(role, [key.keyid], 1, True, paths=[f"{role}/*"]) for role, key in keys.items()},
        )), "1.targets.json")
        payload = b"fixture target bytes"
        releases = {"lib": ("10.0.0", "2.0.0"), "other": ("1.2.3",)}

        def generation(version, other_signer="other"):
            references = {"targets.json": parent}
            for role, versions in releases.items():
                targets = {f"{role}/{release}.coven": TargetFile.from_data(f"{role}/{release}.coven", payload, ["sha384"])
                           for release in versions}
                references[f"{role}.json"] = self.issue(
                    role if role == "lib" else other_signer,
                    Targets(version=version, expires=self.expiry, targets=targets), f"{version}.{role}.json")
            snapshot = self.issue("snapshot", Snapshot(version=version, expires=self.expiry, meta=references),
                                  f"{version}.snapshot.json")
            self.issue("timestamp", Timestamp(version=version, expires=self.expiry, snapshot_meta=snapshot), "timestamp.json")

        calls = []

        def fetch(route, destination, *, length=None, max_length=None):
            self.assertTrue(route.startswith("/metadata/"), route)
            calls.append(route)
            try:
                wire = (self.metadata / route.removeprefix("/metadata/")).read_bytes()
            except FileNotFoundError:
                raise DownloadRefused(404) from None
            if length is not None:
                self.assertEqual(len(wire), length)
            if max_length is not None:
                self.assertLessEqual(len(wire), max_length)
            destination.write_bytes(wire)

        cache = self.args.root.parent / "catalog"
        cache.mkdir()
        request = {"coordinate": None, "registry": "test", "root": self.args.root_pin, "trust": self.args.trust_pin}
        root, trust = self.args.root.read_bytes(), self.args.trust.read_bytes()
        module = cache.parent / "catalog-selector.wasm"
        self.build("tuf_fetch_select", module, SELECTOR_BUILD)

        def require_pinned_worker(*args):
            self.fail("pinned catalog tried to build its selector")

        select = self.enterContext(selector_worker(require_pinned_worker, HOST, root, trust, request,
                                                  source=module, pin=digest_of(module.read_bytes())))

        def run():
            return refresh(select, cache, root, request, SimpleNamespace(fetch=fetch))

        generation(1)
        self.assertEqual(run(), {
            "registry": "test", "snapshot": digest_of((self.metadata / "1.snapshot.json").read_bytes()),
            "snapshot_version": 1, "timestamp": digest_of((self.metadata / "timestamp.json").read_bytes()),
            "releases": [{"name": name, "version": version, "target": digest_of(payload), "length": len(payload)}
                         for name, version in (("lib", "2.0.0"), ("lib", "10.0.0"), ("other", "1.2.3"))],
        })
        self.assertEqual(set(calls), {"/metadata/2.root.json", "/metadata/timestamp.json", "/metadata/1.snapshot.json",
                                      "/metadata/1.targets.json", "/metadata/1.lib.json", "/metadata/1.other.json"})
        checkpoint = cache / "coven-fetch-history"
        generation(2, other_signer="lib")
        with self.assertRaisesRegex(ValueError, "invalid Witchy TUF catalog result|metadata verification refused"):
            run()
        advanced = checkpoint.read_bytes()
        timestamp = (self.metadata / "timestamp.json").read_bytes()
        snapshot = (self.metadata / "2.snapshot.json").read_bytes()
        self.assertEqual(advanced.split(b"\n", 3)[3], timestamp + snapshot)
        self.assertFalse((cache / "verified-other.json").read_bytes() == (self.metadata / "2.other.json").read_bytes())

        module.write_bytes(b"changed after the first selection")
        calls.clear()
        with self.assertRaisesRegex(ValueError, "TUF metadata verification module differs from its independent pin"):
            run()
        self.assertEqual(calls, [])
        self.assertEqual(checkpoint.read_bytes(), advanced)


if __name__ == "__main__":
    unittest.main()
