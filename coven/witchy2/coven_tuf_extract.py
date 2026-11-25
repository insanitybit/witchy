"""Verify a selected sealed release and return the package it authorized."""
from contextlib import contextmanager
from pathlib import Path
import re
import subprocess
import tempfile

from coven_staging import canonical, read_file
from coven_target import TARGET_BYTES, TRUST_BYTES
from coven_worker import STAGED_FILE_ENTRY_BYTES, check_worker_pin, run_with_lifeline, worker_arguments
from witchy_release import PAYLOAD_BYTES, digest_of, parse_payload

BUILD = ("--heap-pages=256", "--slots=4096", "--stack-bytes=65536")
MODULE_BYTES = 262144
VERIFIED = re.compile(rb"(sha384:[0-9a-f]{96})\n(sha384:[0-9a-f]{96})\n"
                      rb"([a-z][a-z0-9-]{0,63})/([0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9})\nverified\n")


class Extractor:
    def __init__(self, module_args, host):
        self.module_args = module_args
        self.host = host

    def verify(self, bundle, trust, trust_pin, registry, *, name="", version="", target_pin=None):
        with tempfile.TemporaryDirectory(prefix="coven-release-evidence-") as directory:
            (Path(directory) / "target").write_bytes(bundle)
            payload, _, _, verified_name, verified_version = self.extract(directory, {
                "name": name, "version": version, "registry": registry, "trust": trust_pin,
                "target": digest_of(bundle) if target_pin is None else target_pin, "source": ""}, trust)
            return parse_payload(payload, verified_name, verified_version)

    def extract(self, directory, request, trust):
        root = Path(directory).resolve()
        payload, target = root / "package", root / "target"
        target_size = target.stat().st_size
        if not 0 < target_size <= TARGET_BYTES or not 0 < len(trust) <= TRUST_BYTES:
            raise ValueError("Witchy release evidence rejected")
        wire = canonical(request | {"trust_size": len(trust)}) + b"\n" + trust
        command = (self.host, *self.module_args, f"--file={target}", f"--write-file={payload}", "--no-links",
                   "--allocator", "--clock", "--input-stdin", "--output", "--output-bytes=384",
                   f"--input-bytes={len(wire) + 1}", f"--fs-bytes={target_size + PAYLOAD_BYTES + STAGED_FILE_ENTRY_BYTES}",
                   "--fs-entries=1", f"--module-bytes={MODULE_BYTES}",
                   "--memory-pages=400", "--fuel=8000000000", "--deadline-ms=10000")
        try:
            result = run_with_lifeline(subprocess.run, command, input=wire, capture_output=True, timeout=12)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("Witchy release verifier unavailable") from None
        check_worker_pin(result, "TUF extraction")
        if result.returncode == 0 and result.stdout == b"refused registry\n":
            raise ValueError("release trust policy names another registry")
        if result.returncode == 0 and result.stdout == b"refused lock\n":
            raise ValueError("verified package differs from the locked digest")
        verdict = VERIFIED.fullmatch(result.stdout)
        if result.returncode != 0 or verdict is None:
            raise ValueError("Witchy release evidence rejected")
        source, publisher, name, version = (part.decode("ascii") for part in verdict.groups())
        if request["name"] and (name, version) != (request["name"], request["version"]):
            raise ValueError("Witchy release verifier output names another coordinate")
        checked = read_file(payload, PAYLOAD_BYTES)
        if digest_of(checked) != source:
            raise ValueError("Witchy release verifier output differs from its digest")
        return checked, source, publisher, name, version


@contextmanager
def extractor_worker(build, host, *, source=None, pin=None):
    with worker_arguments(build, "tuf_extract", BUILD, MODULE_BYTES, "TUF extraction",
                          source=source, pin=pin) as module_args:
        yield Extractor(module_args, host)
