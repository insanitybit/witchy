"""Production identity policy with a locally controlled GitHub-format test issuer."""
from concurrent.futures import ThreadPoolExecutor
import hmac
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch
import uuid

from cli_test_support import empty_staging, fresh_coven, provision_staging
from coven_approval import b64, unb64
from coven_ci import CHALLENGE, RECEIPT, STAGE, Staging
from coven_ci_http import SIGNER_BUILD, post, runner_signer, signer_worker
from coven_ci_identity import ChallengeWorker, NonceKey
from coven_ci_service import dispatch
from coven_ci_store import TransactionWorker
from coven_staging import candidate, canonical, read_json, transaction
from witchy_release import PAYLOAD_BYTES, digest_of

BUNDLE = b"witchy-package 1\nname demo\nversion 1.0.0\n\npub fn one() -> U64:\n    return 1\n"
ISSUER = "https://token.actions.githubusercontent.com"
CHALLENGE_KEY = b"X" * 32


COMPILER = shutil.which(os.environ.get("WITCHYC", "witchyc"))
HOST = shutil.which(os.environ.get("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))
SIGNED_FIXTURES = importlib.util.find_spec("cryptography")

if SIGNED_FIXTURES and COMPILER and HOST:
    from cryptography.hazmat.primitives.asymmetric import rsa

    # The suite forks each case from this module. All cases use the same test issuer.
    ISSUER_KEY = rsa.generate_private_key(public_exponent=65537, key_size=2048)


# Independent wire encoder for tests of the Witchy signer and verifier.
def proof_input(origin, route, token, nonce, package, digest, body_digest):
    return b"coven-ci-request-1-ml-dsa-87\x00" + canonical({"method": "POST", "uri": origin + route, "identity": digest_of(token.encode("ascii")),
                               "nonce": nonce, "package": package, "release": digest, "body": body_digest})


@unittest.skipUnless(SIGNED_FIXTURES, "install the signed fixture requirements")
@unittest.skipUnless(COMPILER and HOST, "install the Witchy compiler and Wasm host")
class TransportReceipt(unittest.TestCase):
    def test_success_must_report_the_exact_checked_ca_pin(self):
        pin = digest_of(b"approved CA")
        other = digest_of(b"other CA")
        request = {"origin": "https://stage.example", "route": CHALLENGE, "body_size": 0}
        for receipt in (b"verified\n", f"verified {other}\n".encode()):
            with self.subTest(receipt=receipt), patch(
                    "coven_ci_http.run_with_lifeline",
                    return_value=subprocess.CompletedProcess([], 0, receipt + b'{"nonce":"valid"}', b"")):
                with self.assertRaisesRegex(ValueError, "invalid CI HTTPS worker response"):
                    post("host", ("worker.wasm",), "ca.pem", "stage.example:443", request, ca_pin=pin)


class StagingIdentity(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.artifact = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.artifact.cleanup)
        cls.module = Path(cls.artifact.name) / "identity.wasm"
        cls.build = staticmethod(fresh_coven(Path(__file__).with_name("coven")).build_module)
        cls.build("ci_identity", cls.module, ("--heap-pages=128", "--slots=4096", "--stack-bytes=65536"))
        cls.identity = ChallengeWorker(cls.module, digest_of(cls.module.read_bytes()))
        cls.addClassCleanup(cls.identity.close)
        cls.storage_module = Path(cls.artifact.name) / "storage.wasm"
        cls.build("ci_commit", cls.storage_module, ("--heap-pages=128", "--slots=4096", "--stack-bytes=65536"))
        cls.storage = TransactionWorker(cls.storage_module, digest_of(cls.storage_module.read_bytes()), os.path.abspath(HOST))
        cls.addClassCleanup(cls.storage.close)

    def setUp(self):
        from cryptography.hazmat.primitives.asymmetric.mldsa import MLDSA87PrivateKey

        private = tempfile.TemporaryDirectory()
        self.addCleanup(private.cleanup)
        root = Path(private.name)
        self.issuer = ISSUER_KEY
        self.client = MLDSA87PrivateKey.generate()
        jwks = root / "jwks.json"
        key = self.issuer.public_key().public_numbers()
        self.jwk = {"kty": "RSA", "n": b64(key.n.to_bytes(256, "big")), "e": b64(key.e.to_bytes(3, "big")),
                    "kid": "fixture", "alg": "RS256", "use": "sig"}
        jwks.write_bytes(canonical({"keys": [self.jwk]}))
        workflow = "owner/repo/.github/workflows/release.yml"
        sha = "b" * 40
        self.enrolled = {"sub": "repo:owner/repo:environment:release", "repository_id": "123", "repository_owner_id": "456",
                         "environment": "release", "caller_workflow": workflow,
                         "job_workflow_ref": "owner/automation/.github/workflows/stage.yml@" + sha, "job_workflow_sha": sha,
                         "key": b64(self.client.public_key().public_bytes_raw())}
        self.ref = "refs/tags/v1.0.0"
        self.origin = "https://stage.example.test"
        self.config = {"profile": "github-actions-v1", "origin": self.origin, "registry": "test",
                       "jwks_pin": digest_of(jwks.read_bytes()), "packages": {"demo": self.enrolled}}
        config_file = root / "config.json"
        config_file.write_bytes(canonical(self.config))
        self.directory = root / "staging"
        empty_staging(self.directory)
        self.nonce_file = root / "nonce.key"
        with os.fdopen(os.open(self.nonce_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as key_file:
            key_file.write(CHALLENGE_KEY)
        self.staging = Staging(config_file, digest_of(config_file.read_bytes()), jwks, self.directory, self.nonce_file, self.identity, self.storage)
        self.claims = {field: value for field, value in self.enrolled.items() if field not in {"key", "caller_workflow"}}
        self.claims.update(iss=ISSUER, aud=self.staging.audience, ref=self.ref, workflow_ref=workflow + "@" + self.ref,
                           runner_environment="self-hosted", ref_type="tag", event_name="push", sha="c" * 40,
                           run_id="7", run_attempt="1", check_run_id=8)
        self.digest = digest_of(BUNDLE)

    def token(self, **overrides):
        now = int(time.time())
        claims = self.claims | {"iat": now, "nbf": now, "exp": now + 90, "jti": str(uuid.uuid4())} | overrides
        return self.compact(canonical(claims))

    def compact(self, payload, header=None, key=None):
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import padding

        header = header or b'{"alg":"RS256","kid":"fixture","typ":"JWT"}'
        message = f"{b64(header)}.{b64(payload)}"
        signature = (key or self.issuer).sign(message.encode("ascii"), padding.PKCS1v15(), hashes.SHA256())
        return f"{message}.{b64(signature)}"

    def proof(self, token=None, *, key=None, nonce=None, route=STAGE, body=BUNDLE, digest=None, origin=None):
        token = token or self.token()
        nonce = nonce or self.staging.challenge()["nonce"]
        digest = digest or self.digest
        signature = (key or self.client).sign(proof_input(origin or self.origin, route, token, nonce, "demo", digest, digest_of(body)))
        return (token, nonce, "demo", digest, b64(signature))

    def test_runner_proof_matches_the_independent_wire_encoding(self):
        runner = self.directory.parent / "runner.seed"
        runner.touch(mode=0o600)
        runner.write_bytes(self.client.private_bytes_raw())
        module_file = runner.with_name("ci_sign.wasm")
        self.build("ci_sign", module_file, SIGNER_BUILD)

        def require_pinned_worker(*args):
            self.fail("pinned runner signer tried to build its worker")

        signer = self.enterContext(signer_worker(require_pinned_worker, source=module_file,
                                                pin=digest_of(module_file.read_bytes())))
        public = runner_signer(runner, signer, HOST, {"mode": "public-key"})
        self.assertEqual(public, self.enrolled["key"])
        token, nonce = self.token(), self.staging.challenge()["nonce"]
        request = {"mode": "sign", "origin": self.origin, "route": STAGE, "token": token,
                   "nonce": nonce, "package": "demo", "digest": self.digest, "body_size": len(BUNDLE)}
        proof = runner_signer(runner, signer, HOST, request, BUNDLE)
        message = proof_input(self.origin, STAGE, token, nonce, "demo", self.digest, digest_of(BUNDLE))
        self.client.public_key().verify(unb64(proof), message)
        snapshot = Path(signer[0])
        original = snapshot.read_bytes()
        snapshot.write_bytes(original + b" ")
        try:
            with self.assertRaisesRegex(ValueError, "CI runner signer module differs from its independent pin"):
                runner_signer(runner, signer, HOST, request, BUNDLE)
        finally:
            snapshot.write_bytes(original)
        self.assertEqual(self.staging.request(STAGE, token, nonce, "demo", self.digest, proof, BUNDLE)["digest"], self.digest)
        changed = BUNDLE[:-1] + b"x"
        with self.assertRaisesRegex(ValueError, "runner signer refused"):
            runner_signer(runner, signer, HOST, request, changed)
        runner.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "runner key must be a private"):
            runner_signer(runner, signer, HOST, {"mode": "public-key"})

    def test_bound_identity_writes_once_and_fresh_proof_recovers_receipt(self):
        proof = self.proof()
        first = self.staging.request(STAGE, *proof, BUNDLE)
        with self.assertRaisesRegex(ValueError, "replay"):
            self.staging.request(STAGE, *proof, BUNDLE)
        with self.assertRaisesRegex(ValueError, "replay"):
            self.staging.request(STAGE, *self.proof(proof[0]), BUNDLE)
        with self.assertRaisesRegex(ValueError, "replay"):
            self.staging.request(STAGE, *self.proof(nonce=proof[1]), BUNDLE)
        with self.assertRaisesRegex(ValueError, "another job"):
            other = self.proof(self.token(check_run_id="456"), route=RECEIPT, body=b"")
            self.staging.request(RECEIPT, *other, b"")
        again = self.staging.request(RECEIPT, *self.proof(route=RECEIPT, body=b""), b"")
        self.assertEqual(again, first)
        self.assertEqual(again["check_run_id"], "8")
        with transaction(self.directory) as db:
            self.assertEqual(candidate(db, self.digest).payload, BUNDLE)
            self.assertEqual(db.execute("SELECT count(*) FROM ci_receipts").fetchone()[0], 1)

    def test_conflicting_candidate_rolls_back_credential_consumption(self):
        previous = BUNDLE.replace(b"return 1", b"return 2")
        with transaction(self.directory) as db:
            db.execute("INSERT INTO candidates VALUES (?, ?, ?, ?)", (self.digest, "demo", "1.0.0", previous))
        with self.assertRaisesRegex(ValueError, "staged bytes"):
            self.staging.request(STAGE, *self.proof(), BUNDLE)
        with transaction(self.directory) as db:
            self.assertEqual(db.execute("SELECT payload FROM candidates").fetchall(), [(previous,)])
            self.assertEqual(db.execute("SELECT jti FROM ci_requests").fetchall(), [])
            self.assertEqual(db.execute("SELECT document FROM ci_receipts").fetchall(), [])

    def test_existing_receipt_key_remains_recoverable(self):
        old_record = {
            "registry": self.staging.registry, "name": "demo", "version": "1.0.0", "digest": self.digest,
            "workflow": self.claims["workflow_ref"], "workflow_commit": self.enrolled["job_workflow_sha"],
            "revision": self.claims["sha"], "repository_id": self.enrolled["repository_id"],
            "run_id": "7", "run_attempt": "1", "check_run_id": "8", "staged_at": int(time.time()) - 60,
        }
        legacy_key = digest_of(canonical([ISSUER, self.staging.registry, self.enrolled]))
        with transaction(self.directory) as db:
            db.execute("INSERT INTO candidates VALUES (?, ?, ?, ?)", (self.digest, "demo", "1.0.0", BUNDLE))
            db.execute("INSERT INTO ci_receipts VALUES (?, ?, ?, ?, ?, ?)",
                       (self.digest, "demo", legacy_key, "7", "1", canonical(old_record)))
        recovered = self.staging.request(RECEIPT, *self.proof(route=RECEIPT, body=b""), b"")
        self.assertEqual(recovered, old_record)
        with transaction(self.directory) as db:
            self.assertEqual(db.execute("SELECT document FROM ci_receipts").fetchall(), [(canonical(old_record),)])

    def test_wrong_key_body_claims_and_forged_intent_leave_store_empty(self):
        from cryptography.hazmat.primitives.asymmetric.mldsa import MLDSA87PrivateKey

        with self.assertRaisesRegex(ValueError, "enrolled CI key"):
            self.staging.request(STAGE, *self.proof(key=MLDSA87PrivateKey.generate()), BUNDLE)
        with self.assertRaisesRegex(ValueError, "enrolled CI key"):
            self.staging.request(STAGE, *self.proof(), BUNDLE.replace(b"return 1", b"return 2"))
        for claims in ({"aud": "other-origin"}, {"repository_id": "909"}, {"runner_environment": "github-hosted"},
                       {"environment": "preview"}, {"job_workflow_sha": "1" * 40}, {"event_name": "pull_request"},
                       {"iss": "https://wrong.example"}, {"exp": int(time.time()) - 1}):
            with self.subTest(claims=claims), self.assertRaises(ValueError):
                self.staging.request(STAGE, *self.proof(self.token(**claims)), BUNDLE)
        forged = {"parameters": {"name": "demo", "version": "1.0.0", "digest": self.digest},
                  "route": STAGE}
        with self.assertRaisesRegex(ValueError, "identity refused"):
            self.storage.commit(self.directory, self.staging.nonce_key, forged, BUNDLE)
        with transaction(self.directory) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM candidates").fetchone()[0], 0)
            self.assertEqual(db.execute("SELECT count(*) FROM ci_requests").fetchone()[0], 0)

    def legacy_tag(self, prefix, audience=None):
        domain = b"coven-ci-challenge-1\0" + (audience or self.staging.audience).encode("ascii") + b"\0"
        return b64(hmac.digest(CHALLENGE_KEY, domain + prefix.encode("ascii"), "sha384"))

    def test_challenges_use_the_existing_wire_and_refuse_forgery_or_expiry(self):
        before = int(time.time())
        framed = dispatch(self.staging, b"C")
        self.assertEqual(framed[:1], b"\0")
        nonce = read_json(framed[1:])["nonce"]
        prefix, tag = nonce.rsplit(".", 1)
        entropy, expiry = prefix.split(".")
        self.assertEqual(len(unb64(entropy)), 32)
        self.assertEqual(tag, self.legacy_tag(prefix))
        self.assertLessEqual(before + 60, int(expiry))
        self.assertLessEqual(int(expiry), int(time.time()) + 60)
        self.assertNotEqual(self.staging.challenge()["nonce"].split(".")[0], entropy)
        expired = f"{entropy}.{before - 1}"
        other_registry = self.legacy_tag(prefix, f"{self.origin}/v1/ci/other")
        tampered = tag[:-1] + ("A" if tag[-1] != "A" else "B")
        for rejected in (f"{prefix}.{tampered}", f"{prefix}.{other_registry}", f"{expired}.{self.legacy_tag(expired)}"):
            with self.assertRaisesRegex(ValueError, "challenge refused or expired"):
                self.staging.request(STAGE, *self.proof(nonce=rejected), BUNDLE)
        with transaction(self.directory) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM ci_requests").fetchone()[0], 0)
        old_prefix = f"{b64(bytes(32))}.{int(time.time()) + 45}"
        legacy = f"{old_prefix}.{self.legacy_tag(old_prefix)}"
        self.assertEqual(self.staging.request(STAGE, *self.proof(nonce=legacy), BUNDLE)["digest"], self.digest)

    def test_nonce_key_source_is_private_and_fixed_until_restart(self):
        replacement = self.nonce_file.with_name("rotation.key")
        replacement.write_bytes(CHALLENGE_KEY)
        replacement.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "private 32-byte file"):
            NonceKey(replacement)
        replacement.chmod(0o600)
        alias = replacement.with_name("alias.key")
        alias.symlink_to(replacement.name)
        with self.assertRaises(OSError):
            NonceKey(alias)
        replacement.replace(self.nonce_file)
        with self.assertRaisesRegex(ValueError, "changed since startup"):
            self.staging.challenge()

    def test_concurrent_replay_commits_one_candidate(self):
        proof = self.proof()

        def write():
            try:
                return self.staging.request(STAGE, *proof, BUNDLE)["digest"]
            except ValueError as error:
                return str(error)

        with ThreadPoolExecutor(2) as pool:
            self.assertCountEqual(list(pool.map(lambda _: write(), range(2))), [self.digest, "CI replay refused"])

    def test_proof_binds_origin_route_and_exact_candidate(self):
        with self.assertRaisesRegex(ValueError, "enrolled CI key"):
            self.staging.request(STAGE, *self.proof(origin="https://other.example.test"), BUNDLE)
        with self.assertRaisesRegex(ValueError, "enrolled CI key"):
            self.staging.request(STAGE, *self.proof(route=RECEIPT), BUNDLE)
        with self.assertRaisesRegex(ValueError, "signed CI candidate"):
            self.staging.request(RECEIPT, *self.proof(route=RECEIPT), BUNDLE)
        with self.assertRaisesRegex(ValueError, "signed CI candidate"):
            self.staging.request(STAGE, *self.proof(digest=digest_of(b"other")), BUNDLE)
        wrong_version = BUNDLE.replace(b"version 1.0.0", b"version 2.0.0")
        with self.assertRaisesRegex(ValueError, "signed CI candidate"):
            self.staging.request(STAGE, *self.proof(body=wrong_version, digest=digest_of(wrong_version)), wrong_version)
        with transaction(self.directory) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM ci_requests").fetchone()[0], 0)
            self.assertEqual(db.execute("SELECT count(*) FROM candidates").fetchone()[0], 0)

    def test_storage_refuses_malformed_control_frames(self):
        header = b'{"mode":"check-schema"}'
        frame = len(header).to_bytes(3, "big") + header
        cases = {
            "missing prefix": b"",
            "one-byte prefix": frame[:1],
            "two-byte prefix": frame[:2],
            "empty header": bytes(3),
            "oversize header": (2 * 1024 * 1024 + 1).to_bytes(3, "big"),
            "short header": frame[:-1],
            "invalid UTF-8": bytes((0, 0, 1, 255)),
            "unexpected body": frame + b"x",
            "old format": header + b"\n",
        }
        grants = ("--sqlite-read=ci_requests,ci_receipts,candidates",)
        for case, data in cases.items():
            with self.subTest(case=case):
                response = self.storage._exchange(self.directory, self.staging.nonce_key, data, grants)
                self.assertEqual(response, b"refused identity\n")
        self.storage.check_schema(self.directory, self.staging.nonce_key)

    def test_a_full_size_binary_candidate_is_stored_byte_for_byte(self):
        tail = (b"\n\x00\xff" * PAYLOAD_BYTES)[:PAYLOAD_BYTES - len(BUNDLE)]
        body = BUNDLE + tail
        digest = digest_of(body)
        with self.assertRaisesRegex(ValueError, "invalid private CI"):
            self.staging.request(STAGE, *self.proof(body=body, digest=digest), body + b"x")
        receipt = self.staging.request(STAGE, *self.proof(body=body, digest=digest), body)
        self.assertEqual(receipt["digest"], digest)
        with transaction(self.directory) as db:
            self.assertEqual(candidate(db, digest).payload, body)

    def test_witchy_refuses_tokens_with_wrong_signatures_or_ambiguous_wire(self):
        from cryptography.hazmat.primitives.asymmetric import rsa

        header, payload, signature = self.token().split(".")
        raw = unb64(payload)
        wrong_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        variants = (
            self.compact(raw, key=wrong_key),
            self.compact(raw, b'{"alg":"RS256","kid":"unknown"}'),
            self.compact(raw, b'{"alg":"RS256","kid":"fixture","jku":"https://wrong.example"}'),
            self.compact(raw, b'{"alg":"none","alg":"RS256","kid":"fixture"}'),
            self.compact(raw[:-1] + b',"jti":"two"}'),
            self.token(iss="https://wrong.example"),
            self.token(aud="https://unrelated.example/v1/ci/test"),
            self.token(aud=[self.staging.audience]),
            self.token(nbf=int(time.time()) + 60),
            self.token(exp=int(time.time()) + 901),
            self.token(run_attempt="01"),
            f"{header}.{payload}.{signature}=",
        )
        for index, token in enumerate(variants):
            with self.subTest(case=index), self.assertRaisesRegex(ValueError, "CI identity|invalid private CI"):
                self.staging.request(STAGE, *self.proof(token), BUNDLE)
        with transaction(self.directory) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM ci_requests").fetchone()[0], 0)

    def test_issuer_key_set_and_module_pin_are_checked(self):
        for keys in ([self.jwk, self.jwk], [self.jwk | {"n": b64(bytes([129]) * 128)}],
                     [self.jwk | {"e": "AQ"}], [self.jwk | {"use": "enc"}]):
            wire = canonical({"keys": keys})
            config = canonical(self.config | {"jwks_pin": digest_of(wire)})
            with self.subTest(keys=len(keys)), self.assertRaisesRegex(ValueError, "key set"):
                self.identity.check_configuration(self.staging.nonce_key, config.decode("ascii"), wire.decode("ascii"))
        with self.assertRaisesRegex(ValueError, "independent pin"):
            ChallengeWorker(self.module, digest_of(b"different module"))

    def test_storage_module_requires_an_independent_pin(self):
        with self.assertRaisesRegex(ValueError, "storage module differs"):
            TransactionWorker(self.storage_module, "sha384:" + "0" * 96, os.path.abspath(HOST))

    def test_host_checks_the_identity_and_storage_module_at_each_launch(self):
        def challenge():
            self.identity.challenge(self.staging.nonce_key, self.staging.audience)

        def check_schema():
            self.storage.check_schema(self.directory, self.staging.nonce_key)

        for role, snapshot, invoke in (
            ("identity", Path(self.identity.command[1]), challenge),
            ("storage", self.storage.module, check_schema),
        ):
            with self.subTest(role=role):
                original = snapshot.read_bytes()
                snapshot.write_bytes(original + b" ")
                try:
                    with self.assertRaisesRegex(ValueError, f"CI {role} module differs from its independent pin"):
                        invoke()
                finally:
                    snapshot.write_bytes(original)
                invoke()

    def test_schema_provisioning_upgrades_existing_staging_before_service_start(self):
        with transaction(self.directory) as db:
            db.execute("DROP TABLE ci_requests")
            db.execute("CREATE TABLE ci_requests (jti TEXT PRIMARY KEY)")
            db.execute("INSERT INTO candidates VALUES (?, 'demo', '1.0.0', ?)", (self.digest, BUNDLE))
        with self.assertRaisesRegex(ValueError, "staging schema unavailable"):
            self.storage.check_schema(self.directory, self.staging.nonce_key)
        with transaction(self.directory) as db:
            db.execute("DROP TABLE ci_receipts")
            db.execute("DROP TABLE ci_requests")
        with self.assertRaisesRegex(ValueError, "staging schema unavailable"):
            self.storage.check_schema(self.directory, self.staging.nonce_key)
        provision_staging(self.directory)
        self.storage.check_schema(self.directory, self.staging.nonce_key)
        receipt = self.staging.request(STAGE, *self.proof(), BUNDLE)
        provision_staging(self.directory)
        with transaction(self.directory) as db:
            self.assertEqual(db.execute("SELECT payload FROM candidates").fetchall(), [(BUNDLE,)])
            self.assertEqual(db.execute("SELECT digest FROM ci_receipts").fetchall(), [(receipt["digest"],)])
            self.assertEqual(db.execute("SELECT count(*) FROM ci_requests").fetchone(), (1,))

    def test_pinned_enrollment_is_validated_in_witchy(self):
        for package, change in (("Bad/Name", {}), ("demo", {"caller_workflow": "org/repo/.github/actions/ci.yml"}),
                                ("demo", {"job_workflow_ref": self.enrolled["job_workflow_ref"].split("@")[0] + "@main"}),
                                ("demo", {"key": self.enrolled["key"][:-1]})):
            with self.subTest(package=package, change=tuple(change)), self.assertRaisesRegex(ValueError, "pinned enrollments"):
                config = canonical(self.config | {"packages": {package: self.enrolled | change}})
                self.identity.check_configuration(self.staging.nonce_key, config.decode("ascii"), self.staging.keys)

    def test_transaction_descriptor_does_not_bypass_authorization(self):
        prefix = ("\n".join(("S", *self.proof())) + "\n").encode("ascii")
        self.assertEqual(dispatch(self.staging, b"GET /\r\n\r\n"), b"\1")
        self.assertEqual(dispatch(self.staging, prefix + BUNDLE + b"changed"), b"\1")
        with transaction(self.directory) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM ci_requests").fetchone()[0], 0)
        reply = dispatch(self.staging, prefix + BUNDLE)
        self.assertEqual(reply[:1], b"\0")
        self.assertIn(self.digest.encode("ascii"), reply)
        self.assertEqual(dispatch(self.staging, prefix + BUNDLE), b"\1")

    def test_invalid_identity_configuration_refuses_to_start(self):
        root = self.directory.parent
        config = root / "config.json"
        unused = root / "refused-staging"
        cases = (({"profile": "unconfigured-issuer"}, "identity configuration"),
                 ({"origin": "http://localhost"}, "identity configuration"),
                 ({"registry": "../test"}, "identity configuration"),
                 ({"jwks_pin": digest_of(b"different key set")}, "key set digest"))
        for fields, error in cases:
            data = canonical(self.config | fields)
            config.write_bytes(data)
            with self.subTest(fields=fields), self.assertRaisesRegex(ValueError, error):
                Staging(config, digest_of(data), root / "jwks.json", unused, self.nonce_file, self.identity, self.storage)
            self.assertFalse(unused.exists())
