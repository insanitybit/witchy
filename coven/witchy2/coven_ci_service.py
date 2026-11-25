"""Run Witchy HTTPS with a private descriptor for the staging transaction."""
import os
from pathlib import Path
import sqlite3
import tempfile

from coven_ci import RECEIPT, STAGE, TOKEN, TOKEN_BYTES, Staging
from coven_ci_identity import NONCE, ChallengeWorker
from coven_ci_store import TransactionWorker
from coven_https import require_loopback, run_listener
from coven_staging import canonical, read_file
from coven_target import SIGNATURE_BYTES
from witchy_release import DIGEST, NAME, PAYLOAD_BYTES

REQUEST_LIMIT = PAYLOAD_BYTES + 16384
REPLY_LIMIT = 16384
PROOF_LENGTH = (SIGNATURE_BYTES * 4 + 2) // 3


def staging_from_environment(storage):
    required = ("COVEN_CI_CONFIG", "COVEN_CI_CONFIG_PIN", "COVEN_CI_JWKS", "COVEN_CI_STAGING",
                "COVEN_CI_NONCE_KEY", "COVEN_CI_IDENTITY_MODULE", "COVEN_CI_IDENTITY_PIN")
    missing = [name for name in required if not os.environ.get(name)]
    if missing:
        raise ValueError("missing CI service settings " + ", ".join(missing))
    identity = ChallengeWorker(Path(os.environ["COVEN_CI_IDENTITY_MODULE"]), os.environ["COVEN_CI_IDENTITY_PIN"])
    try:
        return Staging(Path(os.environ["COVEN_CI_CONFIG"]), os.environ["COVEN_CI_CONFIG_PIN"],
                       Path(os.environ["COVEN_CI_JWKS"]), Path(os.environ["COVEN_CI_STAGING"]),
                       Path(os.environ["COVEN_CI_NONCE_KEY"]), identity, storage)
    except BaseException:
        identity.close()
        raise


def dispatch(staging, message):
    if message == b"C":
        try:
            receipt = b"\0" + canonical(staging.challenge())
        except (ValueError, OSError):
            return b"\2"
        return receipt if len(receipt) <= REPLY_LIMIT else b"\2"
    fields = message.split(b"\n", 6)
    if len(fields) != 7 or fields[0] not in (b"S", b"R"):
        return b"\1"
    try:
        token, nonce, package, digest, proof = (field.decode("ascii") for field in fields[1:6])
    except UnicodeDecodeError:
        return b"\1"
    route = STAGE if fields[0] == b"S" else RECEIPT
    body = fields[6]
    if (len(token) > TOKEN_BYTES or not TOKEN.fullmatch(token) or not NONCE.fullmatch(nonce)
            or not NAME.fullmatch(package) or not DIGEST.fullmatch(digest) or len(proof) != PROOF_LENGTH
            or len(body) > PAYLOAD_BYTES or bool(body) != (route == STAGE)):
        return b"\1"
    try:
        receipt = b"\0" + canonical(staging.request(route, token, nonce, package, digest, proof, body))
    except ValueError:
        return b"\1"
    except (OSError, sqlite3.Error):
        return b"\2"
    return receipt if len(receipt) <= REPLY_LIMIT else b"\2"


def serve(args, host):
    require_loopback(args.listen)
    chain = read_file(args.chain, 65733)
    storage = TransactionWorker(args.storage_module, args.storage_pin, host)
    staging = None
    try:
        staging = staging_from_environment(storage)
        storage.check_schema(staging.directory, staging.nonce_key)
        with tempfile.TemporaryDirectory(prefix="coven-ci-https-", dir=os.path.realpath("/tmp")) as path:
            public = Path(path) / "identity"
            public.mkdir(mode=0o700)
            (public / "chain").write_bytes(chain)
            (public / "authority").write_text(staging.authority + "\n", encoding="ascii")
            launch(staging, host, args, public)
    finally:
        if staging is not None:
            staging.identity.close()
        storage.close()


def launch(staging, host, args, public):
    flags = ("--connection-deadline-ms=8000", "--waiting-connections=16",
             "--memory-pages=42", "--module-bytes=131072", "--fuel=90000000", "--net-bytes=unlimited",
             "--net-bytes-per-connection=2300000", "--fs-bytes=unlimited", "--fs-bytes-per-connection=70000")
    run_listener(host, args.module, args.listen, args.tls_key, public, lambda data: dispatch(staging, data),
                 REQUEST_LIMIT, REPLY_LIMIT, flags, module_pin=args.module_pin)
