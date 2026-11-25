"""Run the Witchy TUF download workers with one origin and one output file."""
from contextlib import contextmanager
from pathlib import Path
import re
from ssl import get_default_verify_paths
import subprocess
import tempfile

from coven_staging import canonical, read_file
from coven_worker import STAGED_FILE_ENTRY_BYTES, check_worker_pin, run_with_lifeline, worker_arguments
from witchy_release import DIGEST, digest_of

CA_FILE_BYTES = 524288
DOWNLOAD_BYTES = 2 << 20
MODULE_BYTES = 262144
BUILD = ("--heap-pages=48", "--slots=4096", "--stack-bytes=65536")
LOOPBACK_BUILD = ("--heap-pages=40", "--slots=512", "--stack-bytes=65536")
STATUS = re.compile(rb"refused ([45][0-9]{2})\n")


def trust_path(explicit):
    chosen = explicit if explicit is not None else get_default_verify_paths().cafile
    if chosen is None:
        raise ValueError("no default PEM CA file; provide --ca-file")
    return Path(chosen).resolve(strict=True)


def read_trust_anchors(path, ca_pin):
    anchors = read_file(path, CA_FILE_BYTES)
    if ca_pin is not None and (not DIGEST.fullmatch(ca_pin) or digest_of(anchors) != ca_pin):
        raise ValueError("HTTPS CA bundle does not match its pin")
    return anchors


class DownloadRefused(ValueError):
    def __init__(self, status):
        super().__init__(f"mirror returned HTTP {status}")
        self.status = status


class Download:
    def __init__(self, root, module_args, anchors, host, url, *, ca_pin=None):
        self.origin = url.geturl()
        self.root = root
        self.ca_pin = ca_pin
        trust = () if anchors is None else (f"--file={anchors}", "--clock", "--rand")
        # Wasmtime snapshots and charges the staged CA file once at entry.
        self.file_overhead = STAGED_FILE_ENTRY_BYTES + (0 if anchors is None else anchors.stat().st_size)
        self.command = (host, *module_args, *trust, f"--net={url.hostname}:{url.port or 443}", "--no-links",
                        "--input-stdin", "--output", "--allocator", "--input-bytes=1025",
                        "--output-bytes=32", "--fs-entries=1",
                        f"--module-bytes={MODULE_BYTES}", "--memory-pages=64", "--net-bytes=3145728",
                        "--fuel=300000000", "--deadline-ms=15000")

    @contextmanager
    def open(self, path):
        with tempfile.TemporaryDirectory(prefix="response-", dir=self.root) as directory:
            body = Path(directory) / "body"
            self.fetch(path, body)
            with body.open("rb") as source:
                yield source

    def fetch(self, path, destination, *, length=None, max_length=None):
        if length is not None and max_length is not None:
            raise ValueError("TUF download accepts one length limit")
        request = {"origin": self.origin, "path": path}
        if self.ca_pin is not None:
            request["ca_pin"] = self.ca_pin
        field, limit = ("length", length) if length is not None else ("max_length", max_length)
        if limit is None:
            limit = DOWNLOAD_BYTES
        else:
            if type(limit) is not int or not 1 <= limit <= DOWNLOAD_BYTES:
                raise ValueError("invalid TUF download length limit")
            request[field] = limit
        command = (*self.command, f"--fs-bytes={self.file_overhead + limit}", f"--write-file={destination}")
        try:
            result = run_with_lifeline(subprocess.run, command, input=canonical(request) + b"\n",
                                       capture_output=True, timeout=17)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("Witchy TUF transport unavailable") from None
        if result.returncode == 0:
            if self.ca_pin is not None and result.stdout == b"refused 7\n":
                raise ValueError("HTTPS CA bundle does not match its pin")
            if refused := STATUS.fullmatch(result.stdout):
                raise DownloadRefused(int(refused[1]))
            if result.stdout == b"ok\n":
                return
        check_worker_pin(result, "TUF download")
        raise ValueError("Witchy TUF download refused")


@contextmanager
def downloader(build, host, url, ca_file, *, ca_pin=None, source=None, pin=None):
    with tempfile.TemporaryDirectory(prefix="coven-tuf-download-") as directory:
        root = Path(directory).resolve()
        anchors = None
        if url.scheme == "https":
            program, options = "tuf_get", BUILD
            wire = read_trust_anchors(trust_path(ca_file), ca_pin)
            ca_pin = ca_pin or digest_of(wire)
            anchors = root / "roots.pem"
            anchors.write_bytes(wire)
        else:
            program, options = "tuf_get_loopback", LOOPBACK_BUILD
        with worker_arguments(build, program, options, MODULE_BYTES, "TUF download",
                              source=source, pin=pin) as module_args:
            yield Download(root, module_args, anchors, host, url, ca_pin=ca_pin)
