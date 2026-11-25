"""Launch the delegated metadata signer with one opaque seed descriptor at a time."""
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import tempfile

from coven_permissions import private_seed
from coven_staging import canonical, read_file
from coven_target import TARGET_BYTES
from coven_worker import check_worker_pin, run_with_lifeline, worker_arguments
from witchy_release import digest_of

MODULE_BYTES = 262144
SIGNED = re.compile(rb"([0-9a-f]{64})\n([0-9a-f]{9254})\n")
ERRORS = {
    b"refused independent key\n": "the delegated signing key must be independent of publisher and approval keys",
    b"refused unassigned key\n": "signing key is outside the TUF delegation",
    b"refused target evidence\n": "delegated target lacks matching verified release evidence",
}
PREPARE_ERRORS = {
    b"refused pin\n": "parent TUF metadata or release trust does not match its pin",
    b"refused role\n": "invalid delegated role, version, or validity period",
    b"refused evidence\n": "Witchy release evidence rejected",
    b"refused target\n": "target is outside the delegation, listed twice, or expires after its parent",
}


@dataclass(frozen=True)
class DelegationSnapshot:
    directory: Path
    bytes_to_read: int
    bundle_count: int

    @property
    def deadline(self):
        return min(600000, 10000 + 200 * self.bundle_count)

    @property
    def fuel(self):
        return 8000000000 * (1 + self.bundle_count)


@contextmanager
def staged_delegation(parent, trust, bundles):
    with tempfile.TemporaryDirectory(prefix="coven-delegated-input-") as directory:
        source = Path(directory).resolve()
        (source / "parent.json").write_bytes(parent)
        (source / "trust.json").write_bytes(trust)
        size = len(parent) + len(trust)
        with (source / "bundles.txt").open("wb") as manifest:
            for bundle in bundles:
                wire = read_file(bundle, TARGET_BYTES)
                pin = digest_of(wire).removeprefix("sha384:")
                target = source / f"target-{pin}"
                if not target.exists():
                    target.write_bytes(wire)
                manifest.write(pin.encode("ascii") + b"\n")
                size += len(wire) + 97
        yield DelegationSnapshot(source, size, len(bundles))


@contextmanager
def delegation_preparer(build, host, *, source=None, pin=None):
    limit = 2 << 20
    options = ("--heap-pages=448", "--slots=65536", "--stack-bytes=65536")
    label = "delegation preparer"
    with worker_arguments(build, "tuf_delegate_prepare", options, MODULE_BYTES, label,
                          source=source, pin=pin) as module:

        def prepare(request, snapshot):
            data = canonical(request | {"count": snapshot.bundle_count})
            (snapshot.directory / "request.json").write_bytes(data)
            size = snapshot.bytes_to_read + len(data) + 16384
            command = (host, *module, f"--dir={snapshot.directory}", "--no-links", "--allocator", "--clock",
                       "--output", f"--output-bytes={limit + 16}", f"--module-bytes={MODULE_BYTES}", "--memory-pages=768",
                       f"--fs-bytes={size}", f"--fuel={snapshot.fuel}", f"--deadline-ms={snapshot.deadline}")
            try:
                result = run_with_lifeline(subprocess.run, command, capture_output=True, timeout=snapshot.deadline / 1000 + 2)
            except (OSError, subprocess.TimeoutExpired):
                raise ValueError("Witchy delegated TUF preparer unavailable") from None
            check_worker_pin(result, label)
            if result.returncode == 0 and result.stdout in PREPARE_ERRORS:
                raise ValueError(PREPARE_ERRORS[result.stdout])
            verdict, _, wire = result.stdout.partition(b"\n")
            if result.returncode or verdict != b"prepared" or not 0 < len(wire) <= limit:
                raise ValueError("Witchy delegated TUF preparer refused")
            return wire

        yield prepare


@contextmanager
def delegation_signer(build, host, *, source=None, pin=None):
    options = ("--heap-pages=448", "--slots=4096", "--stack-bytes=65536")
    label = "delegation signer"
    with worker_arguments(build, "tuf_delegate_sign", options, MODULE_BYTES, label,
                          source=source, pin=pin) as module:

        def sign(seeds, request, unsigned, snapshot):
            data = canonical(request)
            (snapshot.directory / "request.json").write_bytes(data)
            (snapshot.directory / "unsigned.json").write_bytes(unsigned)
            size = snapshot.bytes_to_read + len(data) + len(unsigned) + 16384
            flags = (host, *module, f"--dir={snapshot.directory}", "--no-links", "--allocator", "--clock",
                     "--output", "--output-bytes=9320", f"--module-bytes={MODULE_BYTES}", "--memory-pages=640",
                     f"--fs-bytes={size}", f"--fuel={snapshot.fuel}", f"--deadline-ms={snapshot.deadline}")
            signatures = {}
            for seed in seeds:
                with private_seed(seed, "delegated TUF key") as (fd, _):
                    grant = f"--secret-slot=signer:sign/tuf/targets-ml-dsa-87:fd:{fd}"
                    try:
                        result = run_with_lifeline(subprocess.run, (*flags, grant), capture_output=True,
                                                   pass_fds=(fd,), timeout=snapshot.deadline / 1000 + 2)
                    except (OSError, subprocess.TimeoutExpired):
                        raise ValueError("Witchy delegated TUF signer unavailable") from None
                check_worker_pin(result, label)
                match = SIGNED.fullmatch(result.stdout) if result.returncode == 0 else None
                if match is None:
                    raise ValueError(ERRORS.get(result.stdout, "Witchy delegated TUF signer refused"))
                key, signature = (field.decode("ascii") for field in match.groups())
                if key in signatures:
                    raise ValueError("delegated signing keys must be distinct")
                signatures[key] = signature
            return signatures

        yield sign


@contextmanager
def delegation_finalizer(build, host, *, source=None, pin=None):
    limit = 2 << 20
    options = ("--heap-pages=448", "--slots=65536", "--stack-bytes=65536")
    label = "delegation finalizer"
    with worker_arguments(build, "tuf_delegate_finalize", options, MODULE_BYTES, label,
                          source=source, pin=pin) as module:

        def finalize(request, parent, unsigned, signatures):
            detached = canonical([{"keyid": key, "sig": signatures[key]} for key in sorted(signatures)])
            if len(detached) > limit:
                raise ValueError("delegated metadata exceeds the publication limit")
            with tempfile.TemporaryDirectory(prefix="coven-delegated-final-") as directory:
                inputs = Path(directory).resolve()
                for name, data in (("request.json", canonical(request)), ("parent.json", parent),
                                   ("unsigned.json", unsigned), ("signatures.json", detached)):
                    (inputs / name).write_bytes(data)
                command = (host, *module, f"--dir={inputs}", "--no-links", "--allocator", "--clock",
                           "--output", f"--output-bytes={limit + 16}", f"--module-bytes={MODULE_BYTES}",
                           "--memory-pages=768", f"--fs-bytes={3 * limit + 512}", "--fuel=8000000000",
                           "--deadline-ms=10000")
                try:
                    result = run_with_lifeline(subprocess.run, command, capture_output=True, timeout=12)
                except (OSError, subprocess.TimeoutExpired):
                    raise ValueError("Witchy delegated TUF finalizer unavailable") from None
            check_worker_pin(result, label)
            if result.returncode == 0 and result.stdout == b"refused threshold\n":
                raise ValueError("delegated TUF signature threshold not met")
            verdict, _, wire = result.stdout.partition(b"\n")
            if result.returncode or verdict != b"finalized" or not 0 < len(wire) <= limit:
                raise ValueError("Witchy delegated TUF finalizer refused")
            return wire

        yield finalize
