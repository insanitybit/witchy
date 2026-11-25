"""Check pinned CI settings and issue challenges through the Witchy host."""
from contextlib import contextmanager
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from coven_permissions import private_seed
from coven_staging import canonical, read_file, read_json
from coven_worker import check_worker_pin, run_with_lifeline
from witchy_release import digest_of

NONCE = re.compile(r"[A-Za-z0-9_-]{43}\.[0-9]{1,12}\.[A-Za-z0-9_-]{64}")


class NonceKey:
    def __init__(self, path):
        self.path = Path(path).absolute()
        self.identity = None
        with self.open() as (_, identity):
            self.identity = identity

    @contextmanager
    def open(self):
        with private_seed(self.path, "CI nonce key") as (fd, status):
            identity = status.st_dev, status.st_ino, status.st_mtime_ns, status.st_ctime_ns
            if self.identity is not None and identity != self.identity:
                raise ValueError("CI nonce key changed since startup")
            yield fd, identity


class ChallengeWorker:
    def __init__(self, module, pin):
        wasm = read_file(module, 256 << 10)
        if digest_of(wasm) != pin:
            raise ValueError("CI identity module differs from its independent pin")
        host = shutil.which(os.environ.get("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))
        if host is None:
            raise ValueError("CI identity verification requires witchy-wasmtime-host")
        self.directory = tempfile.TemporaryDirectory(prefix="coven-identity-")
        snapshot = Path(self.directory.name) / "identity.wasm"
        try:
            snapshot.write_bytes(wasm)
        except BaseException:
            self.close()
            raise
        self.command = (os.path.abspath(host), str(snapshot), f"--module-pin={pin}", "--input-stdin", "--output", "--clock", "--rand", "--allocator",
                        "--input-bytes=3145800", "--output-bytes=262165", "--module-bytes=262144",
                        "--memory-pages=160", "--fuel=250000000", "--deadline-ms=3000")

    def close(self):
        self.directory.cleanup()

    def run(self, key, request):
        try:
            with key.open() as (fd, _):
                result = run_with_lifeline(subprocess.run, (*self.command, f"--secret-slot=nonce_key:seal:fd:{fd}"),
                                           input=canonical(request) + b"\n", capture_output=True,
                                           pass_fds=(fd,), timeout=4)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("CI identity verifier unavailable") from None
        if result.returncode:
            check_worker_pin(result, "CI identity")
            raise ValueError("CI identity verifier stopped")
        return result.stdout

    def check_configuration(self, key, config, keys):
        request = {"mode": "check-configuration", "config": config, "keys": keys}
        verdict = self.run(key, request)
        if verdict == b"refused enrollment\n":
            raise ValueError("CI verifier refused the pinned enrollments")
        if verdict == b"refused configuration\n":
            raise ValueError("CI verifier refused the identity configuration")
        if verdict == b"refused key pin\n":
            raise ValueError("CI verifier refused the pinned GitHub OIDC key set digest")
        status, _, document = verdict.partition(b"\n")
        if status != b"valid configuration" or not document.endswith(b"\n"):
            raise ValueError("CI verifier refused the pinned GitHub OIDC key set or request protocol")
        return read_json(document)

    def challenge(self, key, audience):
        wire = self.run(key, {"mode": "issue-challenge", "audience": audience}).decode("ascii")
        nonce = wire.removesuffix("\n")
        if not wire.endswith("\n") or not NONCE.fullmatch(nonce):
            raise ValueError("CI verifier could not issue a challenge")
        return nonce
