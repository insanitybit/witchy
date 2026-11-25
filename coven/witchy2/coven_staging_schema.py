"""Give only the Witchy schema worker table-creation authority."""
from contextlib import contextmanager
import os
from pathlib import Path
import subprocess
import tempfile

from coven_staging import DATABASE, database_path, read_file, require_private_directory
from witchy_release import digest_of

TABLES = "candidates,policy,challenges,browser_approvals,counters,approvals,ci_requests,ci_receipts"
MODULE_BYTES = 32768
BUILD = ("--heap-pages=4", "--slots=64", "--stack-bytes=32768")


def provision_file(directory):
    root = Path(directory)
    root.mkdir(mode=0o700, exist_ok=True)
    require_private_directory(root)
    database = root / DATABASE
    if not os.path.lexists(database):
        if any(path.name != DATABASE for path in root.iterdir()):
            raise ValueError("initialize staging in a separate empty directory")
        try:
            fd = os.open(database, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        except FileExistsError:
            pass
        else:
            os.close(fd)
    return database_path(root)


@contextmanager
def schema_worker(build, host, *, source=None, pin=None):
    with tempfile.TemporaryDirectory(prefix="coven-schema-") as directory:
        module = Path(directory) / "schema.wasm"
        if source is None:
            build("staging_schema", module, BUILD)
        else:
            wasm = read_file(source, MODULE_BYTES)
            if digest_of(wasm) != pin:
                raise ValueError("staging schema module differs from its independent pin")
            module.write_bytes(wasm)

        def initialize(root):
            database = provision_file(root)
            try:
                result = subprocess.run(
                    (host, str(module), f"--sqlite={database}", f"--sqlite-create={TABLES}",
                     "--allocator", "--output", f"--module-bytes={MODULE_BYTES}", "--memory-pages=8",
                     "--output-bytes=64", "--fuel=10000000", "--deadline-ms=5000"),
                    stdin=subprocess.DEVNULL, capture_output=True, env={}, timeout=6,
                )
            except (OSError, subprocess.TimeoutExpired):
                raise ValueError("staging schema worker unavailable") from None
            if result.returncode or result.stdout != b"staging schema ready\n":
                raise ValueError("Witchy could not provision the staging schema")

        yield initialize
