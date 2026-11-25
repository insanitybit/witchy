"""Provision the existing database and launch the scoped Witchy CI transaction."""
from pathlib import Path
import subprocess
import tempfile

from coven_staging import canonical, database_path, read_file, read_json
from coven_worker import check_worker_pin, run_with_lifeline
from witchy_release import PAYLOAD_BYTES, digest_of

HEADER_BYTES = 2 * 1024 * 1024
TRANSACTION_BYTES = 2_799_999

REFUSED = {
    b"refused proof\n": "enrolled CI key signature refused",
    b"refused candidate\n": "invalid signed CI candidate or request body",
    b"refused challenge\n": "CI challenge refused or expired",
    b"refused enrollment\n": "CI package is outside its enrollment",
    b"refused identity\n": "CI identity refused",
}

class TransactionWorker:
    def __init__(self, module, pin, host):
        wasm = read_file(module, 262144)
        if digest_of(wasm) != pin:
            raise ValueError("CI storage module differs from its independent pin")
        self.host, self.pin = host, pin
        self.directory = tempfile.TemporaryDirectory(prefix="coven-ci-storage-")
        self.module = Path(self.directory.name) / "storage.wasm"
        try:
            self.module.write_bytes(wasm)
        except BaseException:
            self.close()
            raise

    def close(self):
        self.directory.cleanup()

    def run(self, directory, key, header, grants, *, body=b""):
        if (not 0 < len(header) <= HEADER_BYTES or len(body) > PAYLOAD_BYTES
                or len(header) + len(body) > TRANSACTION_BYTES):
            raise ValueError("invalid CI storage request size")
        data = len(header).to_bytes(3, "big") + header + body
        return self._exchange(directory, key, data, grants)

    def _exchange(self, directory, key, data, grants):
        path = database_path(directory)
        command = (self.host, str(self.module), f"--module-pin={self.pin}", f"--sqlite={path}", *grants,
                   "--sqlite-request-stdin", "--output", "--allocator", "--clock", "--module-bytes=262144",
                   "--memory-pages=160", f"--input-bytes={len(data)}", "--output-bytes=16400",
                   "--fuel=400000000", "--deadline-ms=5000")
        try:
            with key.open() as (fd, _):
                grant = f"--secret-slot=nonce_key:seal:fd:{fd}"
                result = run_with_lifeline(subprocess.run, (*command, grant), input=data,
                                           capture_output=True, pass_fds=(fd,), timeout=6)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("CI storage worker unavailable") from None
        if result.returncode:
            check_worker_pin(result, "CI storage")
            raise ValueError("CI storage worker stopped")
        return result.stdout

    def check_schema(self, directory, key):
        response = self.run(directory, key, b'{"mode":"check-schema"}',
                            ("--sqlite-read=ci_requests,ci_receipts,candidates",))
        if response != b"committed\nready\n":
            raise ValueError("CI staging schema unavailable. Run coven release init")

    def commit(self, directory, key, request, body):
        tables = "ci_requests,ci_receipts,candidates" if body else "ci_requests,ci_receipts"
        inserts = tables if body else "ci_requests"
        grants = (f"--sqlite-read={tables}", f"--sqlite-insert={inserts}", "--sqlite-delete=ci_requests")
        header = canonical(request | {"body_size": len(body)})
        response = self.run(directory, key, header, grants, body=body)
        if response in REFUSED:
            raise ValueError(REFUSED[response])
        verdict, _, data = response.partition(b"\n")
        if verdict == b"refused" and data.endswith(b"\n") and data.count(b"\n") == 1:
            raise ValueError(data[:-1].decode("ascii"))
        if verdict != b"committed" or not data.endswith(b"\n"):
            raise ValueError("CI storage transaction unavailable")
        receipt = read_json(data)
        if not isinstance(receipt, dict):
            raise ValueError("invalid CI storage receipt")
        return receipt
