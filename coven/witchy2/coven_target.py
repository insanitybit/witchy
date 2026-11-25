"""Coven 1 release targets combine independent publisher and approval evidence."""
from contextlib import contextmanager
import os
from pathlib import Path
import subprocess
import tempfile

from coven_approval import checked_policy, unb64
from coven_permissions import private_seed
from coven_staging import canonical, database_path, read_file, read_json, transaction
from coven_worker import check_worker_pin, run_with_lifeline, worker_arguments
from witchy_release import DIGEST, NAME, PAYLOAD_BYTES, digest_of

TARGET_BYTES = 2 << 20
TRUST_BYTES = 256 << 10
SIGNATURE_BYTES = 4627
FORMAT = "coven-target-1-ml-dsa-87"
TRUST_FIELDS = {"format", "policy", "publishers", "witness"}
SIGNING_DOMAINS = {
    "publisher": "coven target publisher ml-dsa-87 v1",
    "witness": "coven target approval witness ml-dsa-87 v1",
}
STAGING_DOCUMENT_BYTES = 256 << 10
SNAPSHOT_REPLY_BYTES = PAYLOAD_BYTES + 2 * STAGING_DOCUMENT_BYTES + 256
SIGNING_INPUT_BYTES = PAYLOAD_BYTES + 2 * STAGING_DOCUMENT_BYTES + TRUST_BYTES + SIGNATURE_BYTES + 1024
SIGNER_MODULE_BYTES = 262144
SNAPSHOT_MODULE_BYTES = 131072


def public_key(encoded, limit=2592):
    value = unb64(encoded)
    if len(value) != limit:
        raise ValueError("invalid ML-DSA-87 public key")
    return value


def read_trust(path, expected=None):
    return parse_trust(read_file(path, TRUST_BYTES), expected)


def parse_trust(raw, expected=None):
    if expected is not None and (not DIGEST.fullmatch(expected) or digest_of(raw) != expected):
        raise ValueError("release trust file does not match its pin")
    return checked_trust(read_json(raw))


def checked_trust(trust):
    if (not isinstance(trust, dict) or set(trust) not in (TRUST_FIELDS, TRUST_FIELDS | {"policy_history"})
            or trust["format"] != FORMAT or not isinstance(trust["publishers"], dict)
            or any(not NAME.fullmatch(name) for name in trust["publishers"])):
        raise ValueError("invalid Coven release trust file")
    active = checked_policy(trust["policy"])
    history = trust.get("policy_history", [])
    if not isinstance(history, list) or len(history) > 16:
        raise ValueError("invalid Coven policy history")
    previous = 0
    for entry in history:
        if not isinstance(entry, dict) or set(entry) != {"policy", "targets"}:
            raise ValueError("invalid historical policy entry")
        policy = checked_policy(entry["policy"])
        if (not previous < policy["revision"] < active["revision"]
                or any(policy[field] != active[field] for field in ("registry", "origin"))):
            raise ValueError("historical policy does not precede the active policy")
        targets = entry["targets"]
        if (not isinstance(targets, list) or not 1 <= len(targets) <= 2048
                or any(not isinstance(pin, str) or not DIGEST.fullmatch(pin) for pin in targets)):
            raise ValueError("invalid historical policy target pins")
        previous = policy["revision"]
    witness = public_key(trust["witness"])
    for key in trust["publishers"].values():
        if public_key(key) == witness:
            raise ValueError("publisher and approval witness keys must be independent")
    return trust



@contextmanager
def signer_worker(build, host, mode, *, source=None, pin=None):
    program = {"publisher": "tuf_publisher_sign", "witness": "tuf_witness_seal"}[mode]
    domain = SIGNING_DOMAINS[mode].replace(" ", "%20")
    publisher = mode == "publisher"
    heap = 96 if publisher else 256
    memory = 160 if publisher else 400
    input_limit = PAYLOAD_BYTES + 1024 if publisher else SIGNING_INPUT_BYTES
    output_limit = SIGNATURE_BYTES if publisher else TARGET_BYTES + 1
    clock = () if publisher else ("--clock",)
    options = (f"--heap-pages={heap}", "--slots=4096", "--stack-bytes=65536")
    label = f"release {mode} signer"
    with worker_arguments(build, program, options, SIGNER_MODULE_BYTES, label,
                          source=source, pin=pin) as module:

        def sign(key, payload, **evidence):
            header = canonical(evidence | {"body_size": len(payload)}) + b"\n"
            if len(header) + len(payload) > input_limit:
                raise ValueError("release signing input too large")
            with private_seed(key, f"release {mode} key") as (fd, _):
                grant = f"--secret-slot=signer:sign/ml-dsa-87/{domain}:fd:{fd}"
                command = (host, *module, "--input-stdin", "--output", "--allocator", *clock,
                           f"--input-bytes={input_limit}", f"--output-bytes={output_limit}",
                           f"--module-bytes={SIGNER_MODULE_BYTES}", f"--memory-pages={memory}", "--fuel=8000000000",
                           "--deadline-ms=10000", grant)
                try:
                    result = run_with_lifeline(subprocess.run, command, input=header + payload, capture_output=True,
                                               pass_fds=(fd,), timeout=12)
                except (OSError, subprocess.TimeoutExpired):
                    raise ValueError("Witchy release signer unavailable") from None
            check_worker_pin(result, label)
            if result.returncode:
                raise ValueError(f"Witchy release {mode} signer refused")
            if publisher:
                if len(result.stdout) == SIGNATURE_BYTES:
                    return result.stdout
                raise ValueError("Witchy release publisher signer refused")
            if result.stdout == b"refused key\n":
                raise ValueError("wrong approval witness key")
            if result.stdout == b"refused policy\n":
                raise ValueError("active staging policy differs from the release trust file")
            if result.stdout == b"refused digest\n":
                raise ValueError("staged bytes do not match their digest")
            if result.stdout.startswith(b"{") and result.stdout.endswith(b"}\n"):
                return result.stdout[:-1]
            raise ValueError(f"Witchy release {mode} signer refused")

        yield sign

def write_new(path, data):
    path = Path(path)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
        os.link(temporary, path)
    finally:
        os.unlink(temporary)


def sign_package(bundle, key, registry, out, signer):
    write_new(out, signer(key, read_file(bundle, PAYLOAD_BYTES), registry=registry))


@contextmanager
def seal_snapshot_worker(build, host, *, source=None, pin=None):
    options = ("--heap-pages=64", "--slots=4096", "--stack-bytes=65536")
    label = "sealing snapshot"
    with worker_arguments(build, "tuf_seal_snapshot", options, SNAPSHOT_MODULE_BYTES, label,
                          source=source, pin=pin) as module:
        command = (host, *module, "--sqlite-request-stdin", "--output", "--allocator",
                   "--sqlite-read=candidates,policy,approvals", f"--sqlite-reply-bytes={SNAPSHOT_REPLY_BYTES}",
                   "--input-bytes=256", f"--output-bytes={SNAPSHOT_REPLY_BYTES + 9}", f"--module-bytes={SNAPSHOT_MODULE_BYTES}",
                   "--memory-pages=100", "--fuel=400000000", "--deadline-ms=5000")

        def snapshot(path, digest):
            try:
                result = run_with_lifeline(subprocess.run, (*command, f"--sqlite={path}"),
                                           input=canonical({"digest": digest}) + b"\n", capture_output=True, timeout=6)
            except (OSError, subprocess.TimeoutExpired):
                raise ValueError("Witchy staged release snapshot unavailable") from None
            check_worker_pin(result, label)
            if result.returncode:
                raise ValueError("Witchy staged release snapshot stopped")
            status, _, record = result.stdout.partition(b"\n")
            if status == b"refused":
                raise ValueError(record.decode("ascii"))
            if status != b"snapshot":
                raise ValueError("Witchy staged release snapshot unavailable")
            return decode_snapshot(record)

        yield snapshot


def decode_snapshot(record):
    try:
        name, version, package_size, policy_size, approval_size, body = record.split(b"\0", 5)
        package, policy, approval = map(int, (package_size, policy_size, approval_size))
        coordinates = {"name": name.decode("ascii"), "version": version.decode("ascii")}
    except ValueError:
        raise ValueError("invalid staged release snapshot") from None
    if (not 0 < package <= PAYLOAD_BYTES or not 0 < policy <= STAGING_DOCUMENT_BYTES
            or not 0 < approval <= STAGING_DOCUMENT_BYTES or package + policy + approval != len(body)):
        raise ValueError("invalid staged release snapshot")
    return body, coordinates | {"package_size": package, "policy_size": policy, "approval_size": approval}


def seal(directory, digest, publisher_file, trust_file, witness_key, out, signer, snapshot):
    trust_wire = read_file(trust_file, TRUST_BYTES)
    publisher = read_file(publisher_file, SIGNATURE_BYTES)
    path = database_path(directory)
    # Hold the write reservation until the signed target is published.
    with transaction(path.parent):
        body, evidence = snapshot(path, digest)
        target = signer(witness_key, b"".join((body, trust_wire, publisher)), **evidence,
                        digest=digest, trust_size=len(trust_wire))
        write_new(out, target)
