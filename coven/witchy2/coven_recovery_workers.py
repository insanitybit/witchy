"""Launch the two scoped Witchy workers used by staging backup and recovery."""
from contextlib import contextmanager
from pathlib import Path
import subprocess
import tempfile

from coven_staging import canonical, read_file, read_json
from coven_worker import run_with_lifeline
from witchy_release import digest_of

MODULE_BYTES = 131072
MANIFEST_BUILD = ("--heap-pages=16", "--slots=512", "--stack-bytes=32768")
DATABASE_BUILD = ("--heap-pages=4", "--slots=64", "--stack-bytes=32768")
READABLE = "candidates,policy,challenges,browser_approvals,counters,approvals"
PENDING = "challenges,browser_approvals"


def snapshot_module(build, name, root, options, selection):
    source, expected = selection
    if (source is None) != (expected is None):
        raise ValueError("supply a recovery module and its independent pin together")
    module = root / (name + ".wasm")
    if source is None:
        build(name, module, options)
    else:
        wasm = read_file(source, MODULE_BYTES)
        if digest_of(wasm) != expected:
            raise ValueError(f"{name} module differs from its independent pin")
        module.write_bytes(wasm)
    return module


@contextmanager
def recovery_workers(build, host, *, manifest=(None, None), database=(None, None)):
    with tempfile.TemporaryDirectory(prefix="coven-recovery-workers-") as directory:
        root = Path(directory)
        metadata = snapshot_module(build, "staging_backup_manifest", root, MANIFEST_BUILD, manifest)
        storage = snapshot_module(build, "staging_recovery", root, DATABASE_BUILD, database)
        yield RecoveryWorkers(host, metadata, storage)


class RecoveryWorkers:
    def __init__(self, host, manifest, database):
        self.host, self.manifest, self.database = host, manifest, database

    def run(self, module, input_flag, data, grants, budgets):
        command = (self.host, str(module), input_flag, "--output", "--allocator",
                   f"--module-bytes={MODULE_BYTES}", "--deadline-ms=5000", *budgets, *grants)
        try:
            result = run_with_lifeline(subprocess.run, command, input=data, capture_output=True, timeout=6)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("Witchy staging recovery worker unavailable") from None
        if result.returncode:
            raise ValueError("Witchy staging recovery worker stopped")
        return result.stdout

    def metadata(self, request):
        result = self.run(self.manifest, "--input-stdin", canonical(request) + b"\n", (),
                          ("--input-bytes=8192", "--output-bytes=1024", "--memory-pages=24", "--fuel=25000000"))
        if result == b"refused pin\n":
            raise ValueError("staging backup manifest differs from its independent pin")
        status, _, document = result.partition(b"\n")
        if not document.endswith(b"\n") or document.count(b"\n") != 1:
            raise ValueError("invalid Coven staging backup manifest")
        return status, document[:-1]

    def create_manifest(self, database, history):
        status, document = self.metadata({"mode": "create", "database": database, "review_history": history})
        if not status.startswith(b"sha384:"):
            raise ValueError("invalid Coven staging backup manifest")
        return status.decode("ascii"), document

    def verify_manifest(self, wire, expected):
        status, document = self.metadata({"mode": "verify", "manifest": wire.decode("utf-8"), "pin": expected})
        if status != b"verified":
            raise ValueError("invalid Coven staging backup manifest")
        return read_json(document)

    def check_database(self, path, *, clear_pending=False):
        grants = (f"--sqlite={path.resolve(strict=True)}", f"--sqlite-read={READABLE}")
        if clear_pending:
            grants += (f"--sqlite-delete={PENDING}",)
        result = self.run(self.database, "--sqlite-request-stdin", b"R" if clear_pending else b"C", grants,
                          ("--input-bytes=1", "--output-bytes=64", "--memory-pages=12", "--fuel=10000000"))
        if result != b"recovery database ready\n":
            raise ValueError("invalid Coven staging schema or challenge cleanup refused")
