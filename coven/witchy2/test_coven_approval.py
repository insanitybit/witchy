"""Private release intents and real WebAuthn verification with fixture keys."""
import importlib.util
from concurrent.futures import ThreadPoolExecutor
from functools import cached_property
import json
from hashlib import sha256
import os
import shutil
import sqlite3
from contextlib import closing
from pathlib import Path
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from cli_test_support import empty_staging, fresh_coven, invoke_coven
from coven_candidate_stage import stage_worker
from coven_review_commit import CounterChanged, PolicyChanged, commit_worker, built_inspection, issuer_worker, policy_worker, receipt_reader
from coven_tuf_extract import extractor_worker
from coven_worker import STAGED_FILE_ENTRY_BYTES
import coven_approval as approval
import coven_staging as staging
import coven_tuf_extract as extraction
from witchy_release import PAYLOAD_BYTES, digest_of, pack

BUNDLE = b"witchy-package 1\nname lib\nversion 1.0.0\n\npub fn one() -> U64:\n    return 1\n"


HOST = shutil.which(os.environ.get("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))
COMPILER = shutil.which(os.environ.get("WITCHYC", "witchyc"))


class StagingBackupLimits(unittest.TestCase):
    def test_copy_deadline_aborts_backup_while_sqlite_can_make_progress(self):
        import coven_staging_backup

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, destination = root / "staging", root / "backup"
            source.mkdir(mode=0o700)
            with closing(sqlite3.connect(source / staging.DATABASE)) as database:
                database.execute("PRAGMA page_size=4096")
                database.execute("CREATE TABLE fixture (bytes BLOB)")
                database.execute("INSERT INTO fixture VALUES (zeroblob(1300000))")
                database.commit()
                self.assertGreater(database.execute("PRAGMA page_count").fetchone()[0], 256)
            ticks = iter([0, 31])
            with patch.object(coven_staging_backup, "time", SimpleNamespace(monotonic=lambda: next(ticks))):
                with self.assertRaisesRegex(ValueError, "database backup exceeded its time limit"):
                    coven_staging_backup.backup(source, destination, None)
            self.assertFalse(destination.exists())
            self.assertEqual(list(root.glob(".coven-recovery-*")), [])

    def test_backup_refuses_a_contended_review_checkpoint(self):
        import coven_review_baseline
        import coven_staging_backup

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, destination = root / "staging", root / "backup"
            source.mkdir(mode=0o700)
            (source / staging.DATABASE).touch(mode=0o600)
            with coven_review_baseline.review_history(source):
                history = source / ".review-baseline" / "history"
                history.write_bytes(b"accepted checkpoint")
                with patch.object(coven_review_baseline, "HISTORY_LOCK_SECONDS", 0):
                    with self.assertRaisesRegex(ValueError, "Review baseline state is busy"):
                        coven_staging_backup.backup(source, destination, None)
                self.assertEqual(history.read_bytes(), b"accepted checkpoint")
            self.assertFalse(destination.exists())
            self.assertEqual(list(root.glob(".coven-recovery-*")), [])


@unittest.skipUnless(all(importlib.util.find_spec(name) for name in ("cryptography", "cbor2")),
                     "install the optional approval fixture requirements")
@unittest.skipUnless(COMPILER and HOST, "install the Witchy compiler and Wasm host")
class ReleaseApproval(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cli = fresh_coven(Path(__file__).with_name("coven"))
        cls.commit = cls.enterClassContext(commit_worker(cli.build_module, os.path.abspath(HOST)))
        cls.issue = cls.enterClassContext(issuer_worker(cli.build_module, os.path.abspath(HOST)))

    def setUp(self):
        from cryptography.hazmat.primitives.asymmetric import ec
        from cbor2 import dumps

        private = tempfile.TemporaryDirectory()
        self.addCleanup(private.cleanup)
        self.root = Path(private.name).resolve(strict=True)
        self.stage = self.root / "staging"
        empty_staging(self.stage)
        self.bundle = self.root / "lib.coven"
        self.bundle.write_bytes(BUNDLE)
        self.key = ec.generate_private_key(ec.SECP256R1())
        numbers = self.key.public_key().public_numbers()
        public = dumps({1: 2, 3: -7, -1: 1, -2: numbers.x.to_bytes(32, "big"), -3: numbers.y.to_bytes(32, "big")})
        self.id = approval.b64(b"maintainer-one")
        self.origin = "https://release.example.test"
        self.policy = {"registry": "test", "revision": 1, "origin": self.origin,
                       "credentials": {self.id: {"public_key": approval.b64(public), "packages": ["lib"]},
                                       approval.b64(b"other-maintainer"): {
                                           "public_key": approval.b64(public), "packages": ["other"]}}}
        self.digest = digest_of(BUNDLE)
        with staging.transaction(self.stage) as db:
            db.execute("INSERT INTO policy VALUES (1, ?)", (staging.canonical(self.policy),))
            db.execute("INSERT INTO candidates VALUES (?, ?, ?, ?)", (self.digest, "lib", "1.0.0", BUNDLE))

    @cached_property
    def stage_bundle(self):
        cli = fresh_coven(Path(__file__).with_name("coven"))
        return self.enterContext(stage_worker(cli.build_module, os.path.abspath(HOST)))

    @cached_property
    def policy_commit(self):
        cli = fresh_coven(Path(__file__).with_name("coven"))
        return self.enterContext(policy_worker(cli.build_module, os.path.abspath(HOST)))

    def approve(self, token, assertion):
        return approval.approve(self.stage, token, assertion, self.commit)

    @cached_property
    def read_receipt(self):
        cli = fresh_coven(Path(__file__).with_name("coven"))
        return self.enterContext(receipt_reader(cli.build_module, os.path.abspath(HOST)))

    def test_backup_and_restore_keep_durable_state_without_pending_challenges(self):
        from coven_recovery_workers import recovery_workers
        from coven_staging_backup import backup, restore

        cli = fresh_coven(Path(__file__).with_name("coven"))
        workers = self.enterContext(recovery_workers(cli.build_module, os.path.abspath(HOST)))
        token = approval.challenge(self.stage, self.digest, self.issue, self.commit)["publicKey"]["challenge"]
        receipt = self.approve(token, self.assertion(token, counter=1))
        next_release = BUNDLE.replace(b"version 1.0.0", b"version 1.0.1")
        next_digest = digest_of(next_release)
        with staging.transaction(self.stage) as db:
            db.execute("INSERT INTO candidates VALUES (?, ?, ?, ?)", (next_digest, "lib", "1.0.1", next_release))
        pending = approval.challenge(self.stage, next_digest, self.issue, self.commit)["publicKey"]["challenge"]
        with staging.transaction(self.stage) as db:
            db.execute("INSERT INTO browser_approvals VALUES (?, ?)", (pending, self.id))
        history = self.stage / ".review-baseline" / "history"
        history.parent.mkdir(mode=0o700)
        history.write_bytes(b"review floor fixture")
        archive, recovered = self.root / "archive", self.root / "restored"
        database = self.stage / staging.DATABASE
        with closing(sqlite3.connect(database)) as writer:
            self.assertEqual(writer.execute("PRAGMA journal_mode=WAL").fetchone(), ("wal",))
            writer.execute("INSERT INTO ci_requests VALUES ('committed', 1, 'nonce', 1)")
            writer.commit()
            self.assertGreater(database.with_name(database.name + "-wal").stat().st_size, 0)
            writer.execute("INSERT INTO ci_requests VALUES ('pending', 1, 'different', 1)")
            pin = backup(self.stage, archive, workers)["pin"]
            writer.rollback()
        manifest = (archive / "manifest.json").read_bytes()
        self.assertEqual(pin, digest_of(manifest))
        self.assertEqual(manifest, staging.canonical({
            "format": "coven-staging-backup-1",
            "database": digest_of((archive / staging.DATABASE).read_bytes()),
            "review_history": digest_of(history.read_bytes()),
        }))
        duplicate = manifest[:-1] + b',"review_history":null}'
        with self.assertRaisesRegex(ValueError, "invalid Coven staging backup manifest"):
            workers.verify_manifest(duplicate, digest_of(duplicate))
        with self.assertRaisesRegex(ValueError, "manifest differs"):
            restore(archive, "sha384:" + "0" * 96, recovered, workers)
        self.assertFalse(recovered.exists())
        restore(archive, pin, recovered, workers)
        self.assertEqual(self.read_receipt(recovered, self.digest), receipt)
        self.assertEqual((recovered / ".review-baseline" / "history").read_bytes(), history.read_bytes())
        with staging.transaction(recovered) as db:
            self.assertEqual(db.execute("SELECT jti FROM ci_requests").fetchall(), [("committed",)])
            self.assertEqual(db.execute("SELECT count FROM counters").fetchall(), [(1,)])
            self.assertEqual(db.execute("SELECT challenge FROM challenges").fetchall(), [])
            self.assertEqual(db.execute("SELECT challenge FROM browser_approvals").fetchall(), [])
        with staging.transaction(self.stage) as db:
            self.assertIsNotNone(db.execute("SELECT 1 FROM challenges WHERE challenge=?", (pending,)).fetchone())
            self.assertIsNotNone(db.execute("SELECT 1 FROM browser_approvals WHERE challenge=?", (pending,)).fetchone())
        with self.assertRaisesRegex(ValueError, "destination already exists"):
            restore(archive, pin, recovered, workers)
        saved = archive / staging.DATABASE
        with saved.open("ab") as corrupt:
            corrupt.write(b"changed")
        with self.assertRaisesRegex(ValueError, "database differs"):
            restore(archive, pin, self.root / "refused", workers)
        self.assertFalse((self.root / "refused").exists())

        # The operator pin authenticates the archive, while the database grant
        # prevents its schema from changing durable rows during cleanup.
        with staging.transaction(self.stage) as db:
            db.execute("CREATE TRIGGER erase_approvals AFTER DELETE ON challenges BEGIN DELETE FROM approvals; END")
        triggered = self.root / "with-trigger"
        trigger_pin = backup(self.stage, triggered, workers)["pin"]
        with self.assertRaisesRegex(ValueError, "challenge cleanup refused"):
            restore(triggered, trigger_pin, self.root / "refused", workers)
        self.assertFalse((self.root / "refused").exists())
        self.assertEqual(self.read_receipt(self.stage, self.digest), receipt)

    def set_policy(self, document=None):
        path = self.root / "policy.json"
        path.write_text(json.dumps(self.policy if document is None else document, indent=2))
        return self.policy_commit(self.stage, path)

    def assertion(self, token, *, origin=None, cred=None, rp="release.example.test", flags=5, counter=1, **client_extra):
        from cryptography.hazmat.primitives.asymmetric import ec
        from cryptography.hazmat.primitives import hashes

        ident = cred or self.id
        client = staging.canonical({"type": "webauthn.get", "challenge": token, "origin": origin or self.origin,
                                    **client_extra})
        auth = sha256(rp.encode()).digest() + bytes([flags]) + counter.to_bytes(4, "big")
        signature = self.key.sign(auth + sha256(client).digest(), ec.ECDSA(hashes.SHA256()))
        path = self.root / "assertion.json"
        path.write_text(json.dumps({"id": ident, "rawId": ident, "type": "public-key", "response": {
            "clientDataJSON": approval.b64(client), "authenticatorData": approval.b64(auth),
            "signature": approval.b64(signature)}}))
        return path

    def test_approval_is_once_for_exact_bytes_and_receipt_is_recoverable(self):
        payload = BUNDLE + bytes(1048576 - len(BUNDLE))
        with staging.transaction(self.stage) as db:
            db.execute("DELETE FROM candidates")
        self.bundle.write_bytes(payload)
        self.digest = self.stage_bundle(self.stage, self.bundle)["digest"]
        self.assertEqual(self.digest, digest_of(payload))
        with self.assertRaisesRegex(ValueError, "no maintainer approval"):
            self.read_receipt(self.stage, self.digest)
        multi = pack("lib", "1.0.0", {}, "main.wy",
                     {"main.wy": b'import "helper.wy"\n', "helper.wy": b"// file\n"})
        damaged = self.root / "damaged.coven"
        damaged.write_bytes(multi.replace(b"// file", b"// fake"))
        with self.assertRaisesRegex(ValueError, "invalid canonical package candidate"):
            self.stage_bundle(self.stage, damaged)
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT digest FROM candidates").fetchall(), [(self.digest,)])
        pending = approval.challenge(self.stage, self.digest, self.issue, self.commit)
        token = pending["publicKey"]["challenge"]
        self.assertEqual(token, approval.challenge_for(pending["envelope"]))
        self.assertEqual(pending["publicKey"]["allowCredentials"], [{"id": self.id, "type": "public-key"}])
        alternate = self.root / "alternate.coven"
        alternate.write_bytes(BUNDLE.replace(b"return 1", b"return 2"))
        second = self.stage_bundle(self.stage, alternate)["digest"]
        competing = approval.challenge(self.stage, second, self.issue, self.commit)["publicKey"]["challenge"]
        with self.assertRaisesRegex(ValueError, "assertion refused"):
            self.approve(competing, self.assertion(token))
        receipt = self.approve(token, self.assertion(token, counter=0, extension="x" * 18000))
        self.assertGreater(len(staging.canonical(receipt)), 16384)
        self.assertEqual(receipt["envelope"]["intent"]["digest"], self.digest)
        self.assertEqual(self.read_receipt(self.stage, self.digest), receipt)
        with self.assertRaisesRegex(ValueError, "consumed"):
            self.approve(token, self.assertion(token))
        with self.assertRaisesRegex(ValueError, "already has an approval"):
            self.approve(competing, self.assertion(competing, counter=2))
        with staging.transaction(self.stage) as db:
            db.execute("UPDATE candidates SET payload=? WHERE digest=?", (BUNDLE, self.digest))
        with self.assertRaisesRegex(ValueError, "staged bytes do not match their digest"):
            self.read_receipt(self.stage, self.digest)

    def test_untrusted_origin_wrong_rp_wrong_scope_and_absent_uv_fail(self):
        token = approval.challenge(self.stage, self.digest, self.issue, self.commit)["publicKey"]["challenge"]
        for settings in ({"origin": "https://other.example.test"}, {"rp": "other.example.test"}, {"flags": 1},
                         {"cred": approval.b64(b"other-maintainer")}, {"crossOrigin": True},
                         {"topOrigin": self.origin}):
            with self.subTest(settings=settings), self.assertRaisesRegex(ValueError, "assertion refused"):
                self.approve(token, self.assertion(token, **settings))
        self.approve(token, self.assertion(token))

    def test_enrollment_replaces_only_the_named_scope_and_records_the_signed_counter(self):
        from cbor2 import dumps
        from cryptography.hazmat.primitives.asymmetric import ec
        from coven_enrollment import begin, enrollment_worker, complete, enrollment_snapshot

        cli = fresh_coven(Path(__file__).with_name("coven"))
        commit = self.enterContext(enrollment_worker(cli.build_module, os.path.abspath(HOST)))
        retired = self.id
        with self.assertRaisesRegex(ValueError, "existing package scope"):
            begin(self.stage, ["other"], retired, self.issue, commit)
        request = begin(self.stage, ["lib"], retired, self.issue, commit)
        self.id = approval.b64(b"new-maintainer")
        self.key = ec.generate_private_key(ec.SECP256R1())
        nums = self.key.public_key().public_numbers()
        key = approval.b64(dumps({1: 2, 3: -7, -1: 1, -2: nums.x.to_bytes(32, "big"), -3: nums.y.to_bytes(32, "big")}))
        response = self.root / "enrolled.json"
        token = request["challenge"]
        signed = {"format": request["format"], "challenge": token, "public_key": key,
                  "assertion": json.loads(self.assertion(token, counter=2).read_bytes())}
        response.write_bytes(staging.canonical(signed | {"public_key": approval.b64(b"wrong-key")}))
        with self.assertRaisesRegex(ValueError, "assertion refused"):
            complete(self.stage, response, commit)
        response.write_bytes(staging.canonical(signed))
        with staging.transaction(self.stage) as db:
            db.execute("UPDATE challenges SET expires=0 WHERE challenge=?", (token,))
        with self.assertRaisesRegex(ValueError, "expired"):
            complete(self.stage, response, commit)
        with staging.transaction(self.stage) as db:
            db.execute("UPDATE challenges SET expires=?, policy=? WHERE challenge=?",
                       (request["expires"], "sha384:" + "0" * 96, token))
        with self.assertRaisesRegex(ValueError, "old policy"):
            complete(self.stage, response, commit)
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT document FROM policy").fetchone(), (staging.canonical(self.policy),))
            db.execute("UPDATE challenges SET policy=? WHERE challenge=?", (digest_of(staging.canonical(self.policy)), token))
        snapshot = enrollment_snapshot(self.stage, token, key, signed["assertion"], commit)
        mismatched = snapshot | {"policy": snapshot["policy"] | {"revision": 2}}
        with self.assertRaisesRegex(ValueError, "old policy"):
            commit(self.stage, mismatched)
        result = complete(self.stage, response, commit)
        with self.assertRaisesRegex(ValueError, "consumed"):
            commit(self.stage, snapshot)
        self.assertEqual(result["revision"], 2)
        expected = self.policy["credentials"].copy()
        del expected[retired]
        expected[self.id] = {"public_key": key, "packages": ["lib"]}
        self.assertEqual(result["policy"]["credentials"], expected)
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT count FROM counters WHERE credential=?", (retired,)).fetchone(), (0,))
            self.assertEqual(db.execute("SELECT document FROM policy").fetchone(),
                             (staging.canonical(result["policy"]),))
        with self.assertRaisesRegex(ValueError, "unknown"):
            complete(self.stage, response, commit)
        challenge = approval.challenge(self.stage, self.digest, self.issue, self.commit)["publicKey"]
        self.assertEqual(challenge["allowCredentials"], [{"id": self.id, "type": "public-key"}])
        with self.assertRaisesRegex(ValueError, "assertion refused"):
            self.approve(challenge["challenge"], self.assertion(challenge["challenge"], counter=2))
        self.approve(challenge["challenge"], self.assertion(challenge["challenge"], counter=3))

    @unittest.skipUnless(importlib.util.find_spec("tuf"), "install the optional release requirements")
    def test_delegation_requires_independent_approval_for_the_exact_bundle(self):
        from datetime import datetime, timedelta, timezone
        from unittest.mock import patch
        from cryptography.hazmat.primitives.asymmetric.mldsa import MLDSA87PrivateKey
        from securesystemslib.signer import SSlibKey
        from tuf.api.metadata import DelegatedRole, Delegations, Metadata, TargetFile, Targets
        import coven_target
        import coven_tuf
        from coven_tuf_sign import delegation_finalizer, delegation_preparer, delegation_signer, staged_delegation

        def key(name):
            path = self.root / name
            seed = os.urandom(32)
            with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb") as secret:
                secret.write(seed)
            return path, MLDSA87PrivateKey.from_seed_bytes(seed).public_key()

        publisher, publisher_public = key("publisher")
        witness, witness_public = key("witness")
        delegated, delegated_public = key("delegated")
        second, second_public = key("second delegated")
        trust_file = self.root / "trust.json"
        trust_file.write_bytes(staging.canonical({
            "format": coven_target.FORMAT, "policy": self.policy,
            "publishers": {"lib": approval.b64(publisher_public.public_bytes_raw())},
            "witness": approval.b64(witness_public.public_bytes_raw()),
        }))
        build = fresh_coven(Path(__file__).with_name("coven")).build_module

        def pinned(program, heap, slots=4096):
            module = self.root / f"{program}.wasm"
            build(program, module, (f"--heap-pages={heap}", f"--slots={slots}", "--stack-bytes=65536"))
            return {"source": module, "pin": digest_of(module.read_bytes())}

        def no_build(*args):
            self.fail("the pinned signing workflow tried to invoke the compiler")

        publisher_flags = pinned("tuf_publisher_sign", 96)
        publisher_sign = self.enterContext(coven_target.signer_worker(no_build, os.path.abspath(HOST), "publisher", **publisher_flags))
        witness_seal = self.enterContext(coven_target.signer_worker(no_build, os.path.abspath(HOST), "witness",
                                                                   **pinned("tuf_witness_seal", 256)))
        read_snapshot = self.enterContext(coven_target.seal_snapshot_worker(no_build, os.path.abspath(HOST),
                                                                          **pinned("tuf_seal_snapshot", 64)))
        signature = self.root / "publisher.signature"
        publisher_module = publisher_flags["source"]
        stable_module = publisher_module.read_bytes()
        publisher_module.write_bytes(stable_module + b"x")
        try:
            with self.assertRaisesRegex(ValueError, "release publisher signer module differs from its independent pin"):
                coven_target.sign_package(self.bundle, publisher, "test", signature, publisher_sign)
            self.assertFalse(signature.exists())
        finally:
            publisher_module.write_bytes(stable_module)
        coven_target.sign_package(self.bundle, publisher, "test", signature, publisher_sign)
        claim = {"suite": coven_target.FORMAT, "registry": "test", "name": "lib",
                 "version": "1.0.0", "digest": self.digest}
        publisher_public.verify(signature.read_bytes(),
                                b"coven target publisher ml-dsa-87 v1\0" + staging.canonical(claim))
        multi = pack("lib", "2.1.0", {}, "main.wy",
                     {"main.wy": b'import "helper.wy"\n', "helper.wy": b"// file\n"})
        multi_claim = claim | {"version": "2.1.0", "digest": digest_of(multi)}
        publisher_public.verify(publisher_sign(publisher, multi, registry="test"),
                                b"coven target publisher ml-dsa-87 v1\0" + staging.canonical(multi_claim))
        with self.assertRaisesRegex(ValueError, "publisher signer refused"):
            publisher_sign(publisher, multi.replace(b"// file", b"// fake"), registry="test")
        bundle = self.root / "sealed"
        with self.assertRaisesRegex(ValueError, "missing maintainer approval"):
            coven_target.seal(self.stage, self.digest, signature, trust_file, witness, bundle, witness_seal, read_snapshot)
        self.assertFalse(bundle.exists())
        token = approval.challenge(self.stage, self.digest, self.issue, self.commit)["publicKey"]["challenge"]
        approval_receipt = self.approve(token, self.assertion(token))
        assertion = approval_receipt["assertion"]
        bad_response = assertion["response"] | {"signature": approval.b64(b"bad signature")}
        bad_receipt = approval_receipt | {"assertion": assertion | {"response": bad_response}}
        def refuses_snapshot(statement, replacement, expected):
            select, update = statement
            with staging.transaction(self.stage) as db:
                (original,) = db.execute(select).fetchone()
                db.execute(update, (replacement,))
            try:
                with self.assertRaisesRegex(ValueError, expected):
                    coven_target.seal(self.stage, self.digest, signature, trust_file, witness, bundle, witness_seal, read_snapshot)
                self.assertFalse(bundle.exists())
            finally:
                with staging.transaction(self.stage) as db:
                    db.execute(update, (original,))

        refuses_snapshot(("SELECT document FROM policy", "UPDATE policy SET document=?"),
                         staging.canonical(self.policy) + b" ", "active staging policy differs")
        refuses_snapshot(("SELECT receipt FROM approvals", "UPDATE approvals SET receipt=?"),
                         staging.canonical(bad_receipt), "witness signer refused")
        refuses_snapshot(("SELECT payload FROM candidates", "UPDATE candidates SET payload=?"),
                         BUNDLE + b"// altered\n", "staged bytes do not match their digest")
        with self.assertRaisesRegex(ValueError, "wrong approval witness key"):
            coven_target.seal(self.stage, self.digest, signature, trust_file, delegated, bundle, witness_seal, read_snapshot)
        wrong_registry = self.root / "wrong-registry.signature"
        coven_target.sign_package(self.bundle, publisher, "elsewhere", wrong_registry, publisher_sign)
        with self.assertRaisesRegex(ValueError, "witness signer refused"):
            coven_target.seal(self.stage, self.digest, wrong_registry, trust_file, witness, bundle, witness_seal, read_snapshot)
        self.assertFalse(bundle.exists())
        with self.assertRaisesRegex(ValueError, "publisher signer refused"):
            publisher_sign(publisher, BUNDLE, registry="bad\"registry")
        database = self.stage / staging.DATABASE
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute("PRAGMA journal_mode=WAL").fetchone(), ("wal",))

        def sign_during_reservation(*args, **evidence):
            with closing(sqlite3.connect(database, timeout=0)) as writer:
                with self.assertRaisesRegex(sqlite3.OperationalError, "locked"):
                    writer.execute("BEGIN IMMEDIATE")
            return witness_seal(*args, **evidence)

        coven_target.seal(self.stage, self.digest, signature, trust_file, witness, bundle,
                          sign_during_reservation, read_snapshot)
        target = staging.read_json(bundle.read_bytes())
        self.assertEqual(bundle.read_bytes(), staging.canonical(target))
        self.assertEqual(approval.unb64(target["package"]), BUNDLE)
        self.assertEqual(target["approval"], approval_receipt)
        witness_claim = {"release": claim, "publisher_signature": digest_of(signature.read_bytes()),
                         "approval": digest_of(staging.canonical(approval_receipt)),
                         "attested_at": target["attested_at"]}
        witness_public.verify(approval.unb64(target["witness_signature"]),
                              b"coven target approval witness ml-dsa-87 v1\0" + staging.canonical(witness_claim))

        linked_tmp = self.root / "temp-link"
        linked_tmp.symlink_to(self.root, target_is_directory=True)
        with patch.object(tempfile, "tempdir", str(linked_tmp)):
            finalize = self.enterContext(delegation_finalizer(no_build, os.path.abspath(HOST),
                                                             **pinned("tuf_delegate_finalize", 448, 65536)))
            with staged_delegation(b"", b"", []) as staged:
                self.assertEqual(staged.directory, staged.directory.resolve())

        # Policy rotation reads its own copy while the delegation checks
        # deliberately change the original bundle.
        archive = self.root / "archive.coven"
        archive.write_bytes(bundle.read_bytes())
        pool = self.enterContext(ThreadPoolExecutor(max_workers=1))
        rotation = pool.submit(self.check_archived_release, build, archive, trust_file, publisher_public)

        sign_snapshot = self.enterContext(delegation_signer(no_build, os.path.abspath(HOST),
                                                          **pinned("tuf_delegate_sign", 448)))
        prepare = self.enterContext(delegation_preparer(no_build, os.path.abspath(HOST),
                                                        **pinned("tuf_delegate_prepare", 448, 65536)))

        def sign_delegation(seeds, request, parent, trust, unsigned, paths):
            with staged_delegation(parent, trust, paths) as snapshot:
                return sign_snapshot(seeds, request, unsigned, snapshot)
        public = SSlibKey.from_crypto(delegated_public, scheme="ml-dsa-87/1")
        another = SSlibKey.from_crypto(second_public, scheme="ml-dsa-87/1")
        roles = {name: DelegatedRole(name, [public.keyid, another.keyid], 2, True, paths=[f"{name}/*"])
                 for name in ("lib", "other")}
        parent = Metadata(Targets(expires=datetime.now(timezone.utc) + timedelta(days=2),
                                  delegations=Delegations({public.keyid: public, another.keyid: another}, roles)))
        parent_file = self.root / "parent.json"
        parent_file.write_bytes(parent.to_bytes())
        out = self.root / "lib.json"
        args = SimpleNamespace(parent=parent_file, parent_pin=digest_of(parent_file.read_bytes()), role="lib",
                               trust=trust_file, trust_pin=digest_of(trust_file.read_bytes()), key=[delegated, second],
                               version=1, valid_days=1, bundles=[bundle], out=out)
        before = datetime.now(timezone.utc).replace(microsecond=0)
        original_bundle = bundle.read_bytes()

        def prepare_then_replace(request, snapshot):
            unsigned = prepare(request, snapshot)
            bundle.write_bytes(b"replacement after preparation")
            return unsigned

        try:
            coven_tuf.authorize(args, prepare_then_replace, sign_snapshot, finalize)
        finally:
            bundle.write_bytes(original_bundle)
        metadata = Metadata.from_file(str(out))
        self.assertLessEqual(before + timedelta(days=1), metadata.signed.expires)
        self.assertLessEqual(metadata.signed.expires, datetime.now(timezone.utc) + timedelta(days=1))
        parent.signed.verify_delegate("lib", metadata.signed_bytes, metadata.signatures)
        listed = metadata.signed.targets["lib/1.0.0.coven"]
        listed.verify_length_and_hashes(bundle.read_bytes())
        self.assertEqual(set(listed.hashes), {"sha384"})
        self.assertEqual(set(metadata.signatures), {public.keyid, another.keyid})
        unsigned = Metadata(metadata.signed).to_bytes()
        request = {"role": "lib", "parent_pin": args.parent_pin, "trust_pin": args.trust_pin}
        inputs = (parent_file.read_bytes(), trust_file.read_bytes(), unsigned, [bundle])
        with staged_delegation(*inputs[:2], [bundle, bundle]) as snapshot:
            with self.assertRaisesRegex(ValueError, "listed twice"):
                prepare(request | {"version": 1, "valid_days": 1}, snapshot)
        final_request = {"role": "lib", "parent_pin": args.parent_pin}
        signatures = {key: value.signature for key, value in metadata.signatures.items()}
        self.assertEqual(finalize(final_request, inputs[0], unsigned, signatures), out.read_bytes())
        with self.assertRaisesRegex(ValueError, "signature threshold not met"):
            finalize(final_request, inputs[0], unsigned, {public.keyid: signatures[public.keyid]})
        changed_metadata = staging.read_json(unsigned)
        changed_metadata["signed"]["version"] += 1
        with self.assertRaisesRegex(ValueError, "signature threshold not met"):
            finalize(final_request, inputs[0], staging.canonical(changed_metadata), signatures)
        with self.assertRaisesRegex(ValueError, "independent of publisher and approval keys"):
            sign_delegation([publisher], request, *inputs)
        with self.assertRaisesRegex(ValueError, "signing keys must be distinct"):
            sign_delegation([delegated, delegated], request, *inputs)
        with self.assertRaisesRegex(ValueError, "Witchy delegated TUF signer refused"):
            sign_delegation([delegated], request | {"role": "other"}, *inputs)
        with self.assertRaisesRegex(ValueError, "Witchy delegated TUF signer refused"):
            sign_delegation([delegated], request | {"parent_pin": digest_of(b"other parent")}, *inputs)
        with self.assertRaisesRegex(ValueError, "lacks matching verified release evidence"):
            sign_delegation([delegated], request, *inputs[:-1], [])
        altered = self.root / "altered.coven"
        altered_target = staging.read_json(bundle.read_bytes())
        altered_target["approval"] = bad_receipt
        altered.write_bytes(staging.canonical(altered_target))
        coordinate = "lib/1.0.0.coven"
        bad_offer = TargetFile.from_data(coordinate, altered.read_bytes(), ["sha384"])
        alien = "lib/1.0.1.coven"
        aliased = TargetFile.from_data(alien, bundle.read_bytes(), ["sha384"])
        for offer, evidence_file in ((bad_offer, altered), (aliased, bundle)):
            forged = Metadata(Targets(version=1, expires=metadata.signed.expires, targets={offer.path: offer}))
            with self.assertRaisesRegex(ValueError, "lacks matching verified release evidence"):
                sign_delegation([delegated], request, *inputs[:2], forged.to_bytes(), [evidence_file])
        args.out = self.root / "refused.json"
        args.role = "other"
        with self.assertRaisesRegex(ValueError, "outside the delegation"):
            coven_tuf.authorize(args, prepare, sign_snapshot, finalize)
        self.assertFalse(args.out.exists())
        args.role = "lib"
        value = json.loads(bundle.read_bytes())
        value["approval"]["envelope"]["intent"]["digest"] = "sha384:" + "0" * 96
        bundle.write_bytes(staging.canonical(value))
        with self.assertRaisesRegex(ValueError, "release evidence rejected"):
            coven_tuf.authorize(args, prepare, sign_snapshot, finalize)
        self.assertFalse(args.out.exists())

        rotation.result()

    def check_archived_release(self, build, bundle, trust_file, publisher_public):
        import coven_target
        from coven_policy_rotation import rotation_worker

        trust_pin = digest_of(trust_file.read_bytes())
        with (rotation_worker(build, os.path.abspath(HOST)) as rotate,
              extractor_worker(build, os.path.abspath(HOST)) as extractor):
            new_policy = self.root / "new-policy.json"
            new_policy.write_bytes(staging.canonical(self.policy | {"revision": 2, "credentials": {}}))
            rotated_path = self.root / "rotated-trust.json"
            rotation = SimpleNamespace(trust=trust_file, trust_pin=trust_pin, policy=new_policy,
                                       bundles=[bundle, bundle], out=rotated_path)
            result = rotate(rotation)
            self.assertEqual((result["revision"], result["archived_targets"]), (2, 1))
            trust = coven_target.read_trust(rotated_path, result["digest"])
            self.assertEqual(rotated_path.read_bytes(), staging.canonical(trust))
            self.assertEqual(trust["policy_history"], [{"policy": self.policy, "targets": [digest_of(bundle.read_bytes())]}])
            refused_path = self.root / "refused-rotation.json"
            with self.assertRaisesRegex(ValueError, "trust file does not match its pin"):
                rotate(SimpleNamespace(**(vars(rotation) | {"trust_pin": result["digest"], "out": refused_path})))
            next_rotation = vars(rotation) | {"trust": rotated_path, "trust_pin": result["digest"]}
            with self.assertRaisesRegex(ValueError, "later revision"):
                rotate(SimpleNamespace(**(next_rotation | {"out": refused_path})))
            new_policy.write_bytes(staging.canonical(trust["policy"] | {"revision": 3}))
            changed = self.root / "changed-archive.coven"
            changed.write_bytes(bundle.read_bytes() + b"\n")
            with self.assertRaisesRegex(ValueError, "release evidence rejected"):
                rotate(SimpleNamespace(**(next_rotation | {"bundles": [changed], "out": refused_path})))
            self.assertFalse(refused_path.exists())
            retained_path = self.root / "retained-trust.json"
            retained = rotate(SimpleNamespace(**(next_rotation | {"out": retained_path})))
            self.assertEqual((retained["revision"], retained["archived_targets"]), (3, 1))
            self.assertEqual(coven_target.read_trust(retained_path, retained["digest"])["policy_history"], trust["policy_history"])
            target_bytes, trust_bytes = bundle.read_bytes(), rotated_path.read_bytes()
            target_file = self.root / "target"
            target_file.write_bytes(target_bytes)
            with patch.object(extraction, "run_with_lifeline", wraps=extraction.run_with_lifeline) as launch:
                package, source, publisher_pin, name, version = extractor.extract(self.root, {
                    "name": "lib", "version": "1.0.0", "registry": "test", "trust": result["digest"],
                    "target": digest_of(target_bytes), "source": self.digest}, trust_bytes)
            command = launch.call_args.args[1]
            paths = ("--file=", "--write-file=", "--dir=", "--write-dir=")
            self.assertEqual([flag for flag in command if flag.startswith(paths)],
                             [f"--file={target_file}", f"--write-file={self.root / 'package'}"])
            self.assertIn("--fs-entries=1", command)
            self.assertIn(f"--fs-bytes={len(target_bytes) + PAYLOAD_BYTES + STAGED_FILE_ENTRY_BYTES}", command)
            request, body = launch.call_args.kwargs["input"].split(b"\n", 1)
            self.assertEqual(json.loads(request)["trust_size"], len(trust_bytes))
            self.assertEqual(body, trust_bytes)
            self.assertEqual((package, source, publisher_pin), (BUNDLE, self.digest, digest_of(publisher_public.public_bytes_raw())))
            self.assertEqual((name, version), ("lib", "1.0.0"))
            with self.assertRaisesRegex(ValueError, "release evidence rejected"):
                extractor.verify(target_bytes + b"\n", trust_bytes, result["digest"], "test")
            with patch.object(extraction, "canonical", return_value=request):
                with self.assertRaisesRegex(ValueError, "release evidence rejected"):
                    extractor.verify(target_bytes, trust_bytes + b"\n", result["digest"], "test")
            with self.assertRaisesRegex(ValueError, "trust file does not match its pin"):
                coven_target.read_trust(rotated_path, trust_pin)

    def test_parallel_submissions_only_commit_one_approval(self):
        token = approval.challenge(self.stage, self.digest, self.issue, self.commit)["publicKey"]["challenge"]
        path = self.assertion(token)

        def submit():
            try:
                self.approve(token, path)
                return "accepted"
            except ValueError as error:
                self.assertIn("consumed", str(error))
                return "refused"

        with ThreadPoolExecutor(2) as workers:
            jobs = [workers.submit(submit) for _ in range(2)]
            self.assertCountEqual([job.result() for job in jobs], ["accepted", "refused"])

    def test_commit_rechecks_enrolled_counter_after_snapshot(self):
        token = approval.challenge(self.stage, self.digest, self.issue, self.commit)["publicKey"]["challenge"]
        assertion = staging.read_json(self.assertion(token, counter=2).read_bytes())
        with staging.transaction(self.stage) as db:
            db.execute("UPDATE candidates SET payload=?", (BUNDLE + b"// changed after challenge\n",))
        with self.assertRaisesRegex(ValueError, "staged bytes do not match their digest"):
            approval.approve_assertion(self.stage, token, assertion, self.commit)
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT challenge FROM challenges").fetchall(), [(token,)])
            db.execute("UPDATE candidates SET payload=?", (BUNDLE,))
        request = approval.approval_snapshot(self.stage, token, assertion, False, self.commit)
        self.assertIsNone(request["previous"])
        key = self.policy["credentials"][self.id]["public_key"]
        with staging.transaction(self.stage) as db:
            db.execute("INSERT INTO counters VALUES (?, ?, 1)", (self.id, key))
        with self.assertRaises(CounterChanged):
            self.commit(self.stage, request)
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT count FROM counters").fetchall(), [(1,)])
            self.assertEqual(db.execute("SELECT challenge FROM challenges").fetchall(), [(token,)])
            self.assertEqual(db.execute("SELECT COUNT(*) FROM approvals").fetchone(), (0,))
        receipt = approval.approve_assertion(self.stage, token, assertion, self.commit)
        self.assertEqual(receipt["assertion"], assertion)

    def test_review_transaction_checks_its_module_pin_on_every_run(self):
        from coven_review_commit import pinned_commit

        original_module = Path(self.commit.command[1])
        module_pin = digest_of(original_module.read_bytes())
        with pinned_commit(original_module, module_pin, os.path.abspath(HOST)) as commit:
            self.assertEqual(commit.active_policy(self.stage), self.policy)
            snapshot = Path(commit.command[1])
            original = snapshot.read_bytes()
            snapshot.write_bytes(original + b" ")
            try:
                with self.assertRaisesRegex(ValueError, "review transaction module differs from its independent pin"):
                    commit.active_policy(self.stage)
            finally:
                snapshot.write_bytes(original)
            self.assertEqual(commit.active_policy(self.stage), self.policy)

    def test_review_requires_a_timestamp_after_recording_history(self):
        from cryptography.hazmat.primitives.asymmetric.mldsa import MLDSA87PrivateKey
        from coven_review_baseline import BUILD, pinned_baseline
        from coven_target import FORMAT

        cli = fresh_coven(Path(__file__).with_name("coven"))
        compiled = self.root / "baseline.wasm"
        cli.build_module("tuf_review_baseline", compiled, BUILD)
        module_pin = digest_of(compiled.read_bytes())
        root = self.root / "initial-root.json"
        root.write_bytes(b"{}")
        root_pin = digest_of(root.read_bytes())
        witness = MLDSA87PrivateKey.generate().public_key().public_bytes_raw()
        trust = staging.canonical({"format": FORMAT, "policy": self.policy, "publishers": {},
                                   "witness": approval.b64(witness)})
        with staging.transaction(self.stage) as db:
            release = staging.candidate(db, self.digest)
        mirror = self.root / "mirror"
        (mirror / "metadata").mkdir(parents=True)
        args = self.stage, mirror, release, trust, digest_of(trust), "test"
        with pinned_baseline(compiled, module_pin, os.path.abspath(HOST), root, root_pin) as baseline:
            self.assertIsNone(baseline(*args))
            snapshot = Path(baseline.module_args[0])
            original = snapshot.read_bytes()
            snapshot.write_bytes(original + b" ")
            try:
                with self.assertRaisesRegex(ValueError, "review baseline module differs from its independent pin"):
                    baseline(*args)
            finally:
                snapshot.write_bytes(original)
            self.assertIsNone(baseline(*args))
        history = self.stage / ".review-baseline" / "history"
        self.assertFalse(history.exists())
        history.write_bytes(b"recorded checkpoint")
        with pinned_baseline(compiled, module_pin, os.path.abspath(HOST), root, root_pin) as restarted:
            with self.assertRaisesRegex(ValueError, "baseline rollback refused"):
                restarted(*args)
        self.assertEqual(history.read_bytes(), b"recorded checkpoint")

    def test_browser_review_requires_fresh_scoped_read_and_approval_proofs(self):
        from cryptography.hazmat.primitives.asymmetric.mldsa import MLDSA87PrivateKey
        from coven_release_http import APPROVE, CHALLENGE, INSPECT, Actions
        from coven_release_review import inspect_snapshot
        from coven_target import FORMAT
        from witchy_release import digest_of, parse_payload

        cli = fresh_coven(Path(__file__).with_name("coven"))
        inspection = self.enterContext(built_inspection(cli.build_module, os.path.abspath(HOST)))
        issue_read = self.enterContext(issuer_worker(cli.build_module, os.path.abspath(HOST), browser=True))
        witness = MLDSA87PrivateKey.generate().public_key().public_bytes_raw()
        trust = self.root / "trust.json"
        data = staging.canonical({"format": FORMAT, "policy": self.policy, "publishers": {},
                                  "witness": approval.b64(witness)})
        trust.write_bytes(data)
        mirror = self.root / "mirror"
        mirror.mkdir()
        baseline = lambda *args: None
        actions = Actions(self.stage, mirror, trust, digest_of(data), inspection, baseline, self.commit, issue_read)

        def request(route, data):
            response = actions.dispatch(bytes([route]) + staging.canonical(data))
            return response[0], response[1:]

        commit_module = Path(self.commit.command[1])
        flags = ["--staging-dir", self.stage, "--module", commit_module,
                 "--module-pin", digest_of(commit_module.read_bytes()), self.digest]
        no_compiler = os.environ | {"WITCHYC": str(self.root / "no-compiler")}
        command = [Path(__file__).with_name("coven"), "release"]
        url = invoke_coven([*command, "review-url", *flags], self.root, no_compiler)
        self.assertEqual(url.returncode, 0, url.stderr)
        self.assertEqual(url.stdout.strip(), self.origin + "/review/" + self.digest.removeprefix("sha384:"))
        missing = invoke_coven([*command, "challenge", *flags], self.root, no_compiler)
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("supply both module paths and their independent pins", missing.stderr)
        with self.assertRaisesRegex(ValueError, "invalid review challenge request"):
            issue_read(self.stage, {"operation": "release", "policy": self.policy, "digest": self.digest,
                                    "name": "lib", "version": "1.0.0"})
        with self.assertRaisesRegex(ValueError, "invalid review challenge request"):
            issue_read(self.stage, {"operation": "enroll-maintainer", "policy": self.policy,
                                    "packages": ["lib"], "replace": ""})
        unknown = "sha384:" + "0" * 96
        status, _ = request(CHALLENGE, {"digest": unknown})
        self.assertEqual(status, 0)
        status, response = request(CHALLENGE, {"digest": self.digest})
        self.assertEqual(status, 0)
        options = json.loads(response)["publicKey"]
        self.assertNotIn("allowCredentials", options)
        challenge = options["challenge"]
        unrelated = json.loads(self.assertion(challenge, cred=approval.b64(b"other-maintainer")).read_bytes())
        self.assertEqual(request(INSPECT, {"challenge": challenge, "assertion": unrelated})[0], 1)
        proof = {"challenge": challenge, "assertion": json.loads(self.assertion(challenge).read_bytes())}
        self.assertEqual(request(APPROVE, proof)[0], 1)
        with staging.transaction(self.stage) as db:
            db.execute("ALTER TABLE ci_receipts RENAME TO saved_ci_receipts")
        with staging.transaction(self.stage):
            legacy = Actions(self.stage, mirror, trust, digest_of(data), inspection, baseline, self.commit, issue_read)
        self.assertFalse(legacy.has_provenance)
        release, provenance, snapshot = inspect_snapshot(self.stage, challenge, proof["assertion"], self.commit, False)
        self.assertEqual(release.payload, BUNDLE)
        self.assertIsNone(provenance)
        provenance = {"registry": "test", "digest": self.digest}
        with staging.transaction(self.stage) as db:
            db.execute("ALTER TABLE saved_ci_receipts RENAME TO ci_receipts")
            db.execute("INSERT INTO ci_receipts VALUES (?, 'lib', 'job', '1', '1', ?)",
                       (self.digest, staging.canonical(provenance | {"registry": "other"})))
        with self.assertRaisesRegex(ValueError, "staging provenance does not match"):
            inspect_snapshot(self.stage, challenge, proof["assertion"], self.commit, True)
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT 1 FROM challenges WHERE challenge=?", (challenge,)).fetchone(), (1,))
            db.execute("UPDATE ci_receipts SET document=?", (staging.canonical(provenance),))
        with self.assertRaisesRegex(ValueError, "does not match the candidate"):
            inspection(self.stage, snapshot | {"version": "2.0.0"})
        status, response = request(INSPECT, proof)
        self.assertEqual(status, 0)
        reviewed = json.loads(response)
        self.assertEqual(reviewed["digest"], self.digest)
        self.assertEqual(reviewed["provenance"], provenance)
        self.assertIn("return 1", reviewed["comparison"]["files"][0]["diff"])
        self.assertEqual(request(INSPECT, proof)[0], 1)
        independent = approval.challenge(self.stage, self.digest, self.issue, self.commit)["publicKey"]["challenge"]
        forged = {"challenge": independent, "assertion": json.loads(self.assertion(independent, counter=2).read_bytes())}
        self.assertEqual(request(APPROVE, forged)[0], 1)
        token = reviewed["approval"]["publicKey"]["challenge"]
        self.assertNotEqual(token, challenge)
        for count in (0, 1):
            stale = {"challenge": token, "assertion": json.loads(self.assertion(token, counter=count).read_bytes())}
            self.assertEqual(request(APPROVE, stale)[0], 1)
        proof = {"challenge": token, "assertion": json.loads(self.assertion(token, counter=2).read_bytes())}
        status, response = request(APPROVE, proof)
        self.assertEqual(status, 0)
        self.assertEqual(json.loads(response)["digest"], self.digest)
        self.assertEqual(request(APPROVE, proof)[0], 1)
        token = json.loads(request(CHALLENGE, {"digest": self.digest})[1])["publicKey"]["challenge"]
        proof = {"challenge": token, "assertion": json.loads(self.assertion(token, counter=3).read_bytes())}
        status, response = request(INSPECT, proof)
        self.assertEqual(status, 0)
        self.assertTrue(json.loads(response)["approved"])
        self.assertIsNone(json.loads(response)["approval"])

    def test_diff_distinguishes_hidden_controls_from_literal_escapes(self):
        from coven_release_review import differences
        from witchy_release import parse_payload

        old = parse_payload(BUNDLE + b"// \\u{202e}\n")
        new = parse_payload(BUNDLE + "// \u202e\n".encode())
        diff = differences(old, new)["files"][0]["diff"]
        self.assertIn("-// \\\\u{202e}", diff)
        self.assertIn("+// \\u{202e}", diff)
        self.assertNotIn("\u202e", diff)

    def test_policy_replacement_expiry_and_store_corruption_fail(self):
        with staging.transaction(self.stage) as db:
            db.execute("DELETE FROM policy")
        self.assertEqual(self.set_policy(), {"revision": 1, "digest": digest_of(staging.canonical(self.policy))})
        pending = approval.challenge(self.stage, self.digest, self.issue, self.commit)
        token = pending["publicKey"]["challenge"]
        original = json.loads(staging.canonical(self.policy))
        unchanged = self.set_policy()
        self.assertEqual(unchanged["revision"], 1)
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT challenge FROM challenges").fetchall(), [(token,)])
            db.execute("INSERT INTO counters VALUES (?, ?, 1)",
                       (self.id, original["credentials"][self.id]["public_key"]))
        replacement = json.loads(staging.canonical(original))
        replacement["revision"] = 2
        replacement["credentials"][self.id]["public_key"] = approval.b64(b"another key")
        with self.assertRaisesRegex(ValueError, "needs a new ID"):
            self.set_policy(replacement)
        replacement["credentials"][self.id] = original["credentials"][self.id] | {"packages": ["lib", "lib"]}
        with self.assertRaisesRegex(ValueError, "invalid release policy"):
            self.set_policy(replacement)
        with staging.transaction(self.stage) as db:
            db.execute("UPDATE challenges SET expires=0 WHERE challenge=?", (token,))
        with self.assertRaisesRegex(ValueError, "expired"):
            self.approve(token, self.assertion(token))
        database = staging.database_path(self.stage)
        prior = self.policy_commit.snapshot(database)
        self.policy["revision"] = 2
        self.set_policy()
        with self.assertRaises(PolicyChanged):
            self.policy_commit.install(database, prior, staging.canonical(original | {"revision": 3}))
        with self.assertRaisesRegex(ValueError, "policy changed during challenge issuance"):
            self.issue(self.stage, {"operation": "release", "policy": original, "digest": self.digest,
                                    "name": "lib", "version": "1.0.0"})
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT document FROM policy").fetchone(), (staging.canonical(self.policy),))
            self.assertEqual(db.execute("SELECT count FROM counters").fetchone(), (1,))
        with self.assertRaisesRegex(ValueError, "unknown"):
            self.approve(token, self.assertion(token))
        self.policy["revision"] = 1
        with self.assertRaisesRegex(ValueError, "must increase"):
            self.set_policy()
        with staging.transaction(self.stage) as db:
            db.execute("UPDATE candidates SET payload = ?", (BUNDLE.replace(b"return 1", b"return 2"),))
        with self.assertRaisesRegex(ValueError, "do not match"):
            approval.challenge(self.stage, self.digest, self.issue, self.commit)
        with self.assertRaisesRegex(ValueError, "do not match"):
            self.stage_bundle(self.stage, self.bundle)

    def test_concurrent_policy_replacements_preserve_one_revision_and_refuse_corrupt_policy(self):
        changes = [(self.root / f"{scope}.json", self.policy | {"revision": 2, "credentials": {
            self.id: self.policy["credentials"][self.id] | {"packages": ["lib", scope]}}})
                   for scope in ("alpha", "beta")]
        for path, document in changes:
            path.write_bytes(staging.canonical(document))

        with ThreadPoolExecutor(max_workers=2) as pool:
            attempts = [pool.submit(self.policy_commit, self.stage, path) for path, _ in changes]
        installed = []
        for attempt, (_, document) in zip(attempts, changes):
            if attempt.exception():
                with self.assertRaisesRegex(ValueError, "revision must increase"):
                    attempt.result()
            else:
                self.assertEqual(attempt.result(), {"revision": 2, "digest": digest_of(staging.canonical(document))})
                installed.append(document)
        self.assertEqual(len(installed), 1)
        with staging.transaction(self.stage) as db:
            (saved,) = db.execute("SELECT document FROM policy").fetchone()
            self.assertEqual(saved, staging.canonical(installed[0]))
            db.execute("UPDATE policy SET document=?", (saved + b" ",))
        with self.assertRaisesRegex(ValueError, "invalid release policy"):
            self.set_policy(installed[0] | {"revision": 3})
        with staging.transaction(self.stage) as db:
            self.assertEqual(db.execute("SELECT document FROM policy").fetchone(), (saved + b" ",))
