"""Provision private storage and invoke the scoped Witchy candidate writer."""
from contextlib import contextmanager, nullcontext
import subprocess

from coven_staging import database_path, read_file, read_json
from coven_staging_schema import schema_worker
from coven_worker import worker_module
from witchy_release import PAYLOAD_BYTES

MODULE_BYTES = 131072
BUILD = ("--heap-pages=96", "--slots=4096", "--stack-bytes=65536")


@contextmanager
def stage_worker(build, host, *, source=None, pin=None):
    schema = schema_worker(build, host) if source is None else nullcontext()
    with (worker_module(build, "stage_candidate", BUILD, MODULE_BYTES, "candidate staging",
                        source=source, pin=pin) as module,
          schema as initialize):
        command = (host, str(module), "--sqlite-read=candidates", "--sqlite-insert=candidates",
                   "--sqlite-request-stdin", "--output", "--allocator", f"--module-bytes={MODULE_BYTES}",
                   "--memory-pages=160", f"--input-bytes={PAYLOAD_BYTES + 3}", "--output-bytes=512",
                   "--fuel=400000000", "--deadline-ms=5000")

        def stage(directory, package):
            payload = read_file(package, PAYLOAD_BYTES)
            if initialize is not None:
                initialize(directory)
            database = database_path(directory)
            try:
                result = subprocess.run((*command, f"--sqlite={database}"),
                                        input=len(payload).to_bytes(3, "big") + payload,
                                        capture_output=True, env={}, timeout=6)
            except (OSError, subprocess.TimeoutExpired):
                raise ValueError("candidate staging worker unavailable") from None
            if result.returncode:
                raise ValueError("candidate staging worker stopped")
            if result.stdout == b"invalid candidate\n":
                raise ValueError("invalid canonical package candidate")
            verdict, _, data = result.stdout.partition(b"\n")
            if not data.endswith(b"\n") or data.count(b"\n") != 1:
                raise ValueError("candidate staging transaction unavailable")
            if verdict == b"refused":
                raise ValueError(data[:-1].decode("ascii"))
            if verdict != b"committed":
                raise ValueError("candidate staging transaction unavailable")
            receipt = read_json(data)
            if not isinstance(receipt, dict) or receipt.keys() != {"name", "version", "digest"}:
                raise ValueError("invalid candidate staging result")
            return receipt

        yield stage
