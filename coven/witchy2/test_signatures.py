"""Independent ML-DSA-65 provider for registry policy and fixture tests.

These tests change signed records and inspect decisions made by the Python launcher.
The real executable tests cover engine isolation, key delivery, batch verification
and verification before a write. A machine without cryptography uses the real engine.
"""
from contextlib import contextmanager
from functools import cache

try:
    from cryptography.exceptions import InvalidSignature, UnsupportedAlgorithm
    from cryptography.hazmat.primitives.asymmetric.mldsa import MLDSA65PrivateKey, MLDSA65PublicKey
except ImportError:
    MLDSA65PrivateKey = None

_enabled = False
_packages = False


@cache
def available():
    if MLDSA65PrivateKey is None:
        return False
    try:
        MLDSA65PrivateKey.from_seed_bytes(bytes(32))
    except UnsupportedAlgorithm:
        return False
    return True


@contextmanager
def policy_signatures(*, packages=False):
    global _enabled, _packages
    previous = _enabled, _packages
    _packages = packages
    _enabled = available()
    try:
        yield
    finally:
        _enabled, _packages = previous


def configure(cli):
    if not _enabled:
        return

    def sign(role, key, message):
        if role not in cli.VERIFY_ROLES or message is not None and not message.startswith(cli.domain(role)):
            raise cli.CovenError("wrong message domain in test signer")
        seed, _ = cli.read_seed(key.source)
        try:
            private = MLDSA65PrivateKey.from_seed_bytes(bytes(seed))
        finally:
            seed[:] = bytes(len(seed))
        return private.public_key().public_bytes_raw(), private.sign(message) if message is not None else None

    def verify(items):
        results = []
        for role, public, message, signature in items:
            valid = False
            if cli.verify_record(role, public, message, signature) is not None and message.startswith(cli.domain(role)):
                try:
                    MLDSA65PublicKey.from_public_bytes(public).verify(signature, message)
                    valid = True
                except (InvalidSignature, ValueError):
                    pass
            results.append(valid)
        return results

    if _packages:
        package_fixture(cli)
    cli.witchy_run_signer = sign
    cli.witchy_verify_all = verify


def package_fixture(cli):
    """Transfer ordinary fixture bytes for policy tests; real guests cover storage semantics.

    Only a new, uncontested publication or an intact package fetched into an empty
    directory is represented. Every other request runs the original guest.
    """
    import hashlib
    import tomllib
    from pathlib import Path
    real = cli.run_program

    def run(program, grants, temp):
        def root(grant):
            return Path(tomllib.loads("dir = " + grants["dirs"][grant])["dir"]["root"])
        if program not in ("publish", "fetch") or cli.carry():
            return real(program, grants, temp)
        slot = root("slot")
        if program == "publish" and not any(p.name != cli.SIGNATURE for p in slot.iterdir()):
            bundle = (temp / "bundle").read_bytes()
            record, separator, source = bundle.partition(b"\n\n")
            if separator and root("record") == slot and len(bundle) <= cli.PACKAGE_BYTES:
                digest = "sha384:" + hashlib.sha384(bundle).hexdigest()
                (slot / "package").write_bytes(record + separator)
                (slot / "source.wy").write_bytes(source)
                (slot / "digest").write_text(digest)
                return "published " + digest
        if program == "fetch" and not any(root("cache").iterdir()):
            files = [slot / n for n in ("package", "source.wy", "digest")]
            if all(p.is_file() and not p.is_symlink() for p in files):
                record, source = (p.read_bytes() for p in files[:2])
                digest = "sha384:" + hashlib.sha384(record + source).hexdigest()
                name, version, recorded, locked = (temp / "expected").read_text().split("\n")
                if (len(record) + len(source) <= cli.PACKAGE_BYTES and files[2].read_text() == recorded == digest
                        and locked in ("", digest)):
                    try:
                        cli.checked_release(record, source, name, version)
                    except cli.CovenError:
                        return real(program, grants, temp)
                    (root("cache") / "package").write_bytes(record)
                    (root("cache") / "source.wy").write_bytes(source)
                    return "verified " + digest
        return real(program, grants, temp)
    cli.run_program = run
