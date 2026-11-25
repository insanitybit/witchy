"""Launch the independently pinned Witchy reviewer with read-only mirror access."""
from contextlib import contextmanager
import os
from pathlib import Path
import re
import subprocess
import tempfile

from coven_permissions import extended_acl, lock_exclusive
from coven_staging import canonical, read_file
from coven_tuf_select import HISTORY_BYTES, write_cache
from coven_worker import check_worker_pin, run_with_lifeline, snapshot_worker_arguments
from witchy_release import PAYLOAD_BYTES, digest_of, parse_payload

BUILD = ("--heap-pages=448", "--slots=65536", "--stack-bytes=65536")
HISTORY_LOCK_SECONDS = 30
RESULT = re.compile(rb"([1-9][0-9]{0,6})\n(?:absent\n|([0-9.]{5,29})\n(sha384:[0-9a-f]{96})\nverified\n)")


@contextmanager
def review_history(staging):
    directory = staging / ".review-baseline"
    directory.mkdir(mode=0o700, exist_ok=True)
    descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        mode = os.fstat(descriptor)
        if mode.st_uid != os.getuid() or mode.st_mode & 0o077 or extended_acl(descriptor):
            raise ValueError("review baseline state requires an owner-only directory")
        try:
            lock_exclusive(descriptor, HISTORY_LOCK_SECONDS)
        except TimeoutError:
            raise ValueError("Review baseline state is busy. Retry when the current command finishes.") from None
        try:
            recorded = read_file("history", HISTORY_BYTES, owner_writes_only=True, dir_fd=descriptor)
        except FileNotFoundError:
            recorded = b""
        else:
            if not recorded:
                raise ValueError("empty review baseline checkpoint")
        yield descriptor, recorded
    finally:
        os.close(descriptor)


class Baseline:
    def __init__(self, module_args, host, root, root_pin):
        self.module_args, self.host = module_args, host
        self.root, self.root_pin = root, root_pin

    def __call__(self, staging, mirror, release, trust, trust_pin, registry):
        with review_history(staging) as (parent, recorded):
            checkpoint, previous = self.compare(mirror, release, trust, trust_pin, registry, recorded)
            if checkpoint is not None and checkpoint != recorded:
                write_cache(parent, "history", checkpoint)
            return previous

    def compare(self, mirror, release, trust, trust_pin, registry, recorded):
        request = canonical({"name": release.name, "version": release.version, "registry": registry,
                             "root": self.root_pin, "trust": trust_pin, "root_size": len(self.root),
                             "trust_size": len(trust), "floor_size": len(recorded)}) + b"\n"
        with tempfile.TemporaryDirectory(prefix="coven-review-baseline-") as directory:
            output = Path(directory).resolve() / "package"
            command = (self.host, *self.module_args, f"--dir={mirror}", f"--write-file={output}", "--no-links",
                       "--clock", "--allocator", "--input-stdin", f"--input-bytes={775168 + HISTORY_BYTES}", "--output",
                       "--output-bytes=256", "--fs-bytes=150000000", "--module-bytes=262144",
                       "--memory-pages=768", "--fuel=16000000000", "--deadline-ms=10000")
            try:
                result = run_with_lifeline(subprocess.run, command, input=request + self.root + trust + recorded,
                                           capture_output=True, timeout=12)
            except (OSError, subprocess.TimeoutExpired):
                raise ValueError("Witchy comparison baseline unavailable") from None
            check_worker_pin(result, "review baseline")
            if result.returncode == 0 and result.stdout == b"rollback\n":
                raise ValueError("Witchy comparison baseline rollback refused")
            if result.returncode == 0 and result.stdout == b"unpublished\n" and not recorded:
                return None, None
            verified = RESULT.fullmatch(result.stdout) if result.returncode == 0 else None
            if verified is None:
                raise ValueError("Witchy comparison baseline refused")
            checkpoint_size, version, expected = verified.groups()
            length = int(checkpoint_size)
            data = read_file(output, HISTORY_BYTES + PAYLOAD_BYTES)
            if length > min(len(data), HISTORY_BYTES):
                raise ValueError("invalid review baseline checkpoint")
            checkpoint, payload = data[:length], data[length:]
            if version is None and not payload:
                return checkpoint, None
            if version is None or digest_of(payload).encode("ascii") != expected:
                raise ValueError("Witchy comparison baseline differs from its digest")
            return checkpoint, parse_payload(payload, release.name, version.decode("ascii"))


@contextmanager
def pinned_baseline(module, pin, host, root, root_pin):
    with snapshot_worker_arguments(None, "tuf_review_baseline", BUILD, 262144, "review baseline",
                                   source=module, pin=pin) as module_args:
        initial = read_file(root, 512000)
        if digest_of(initial) != root_pin:
            raise ValueError("review initial TUF root does not match its pin")
        yield Baseline(module_args, host, initial, root_pin)
