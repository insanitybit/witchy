"""Probe a public HTTPS mirror through the pinned Witchy client worker."""
from pathlib import Path
import re
import subprocess
import tempfile

from coven_https_client import read_trust_anchors
from coven_staging import canonical
from coven_tuf import origin
from coven_worker import STAGED_FILE_ENTRY_BYTES, check_worker_pin, run_with_lifeline
from witchy_release import DIGEST

MODULE_BYTES = 131072
REPLY = re.compile(rb'\{"content_length":([1-9][0-9]{0,19}),"status":200\}\n')


def check(args, host):
    url = origin(args.origin)
    if url.scheme != "https":
        raise ValueError("mirror check requires an HTTPS origin")
    if not DIGEST.fullmatch(args.module_pin):
        raise ValueError("invalid mirror check module SHA-384 pin")
    anchors = read_trust_anchors(args.ca_file.resolve(strict=True), args.ca_pin)
    request = canonical({"origin": args.origin, "route": args.route, "ca_pin": args.ca_pin}) + b"\n"
    with tempfile.TemporaryDirectory(prefix="coven-mirror-check-") as directory:
        ca = Path(directory).resolve() / "ca.pem"
        ca.write_bytes(anchors)
        command = (host, str(args.module.absolute()), f"--module-pin={args.module_pin}", "--no-links", "--clock", "--rand", "--allocator", "--output",
                   "--input-stdin", "--input-bytes=1025", "--output-bytes=64",
                   f"--net={url.hostname}:{url.port or 443}", f"--file={ca}",
                   "--fs-entries=1", f"--fs-bytes={len(anchors) + STAGED_FILE_ENTRY_BYTES}",
                   f"--module-bytes={MODULE_BYTES}", "--memory-pages=64", "--net-bytes=131072",
                   "--fuel=300000000", "--deadline-ms=5000")
        try:
            result = run_with_lifeline(subprocess.run, command, input=request, capture_output=True, timeout=8)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("Witchy mirror check worker unavailable") from None
    check_worker_pin(result, "mirror check")
    if result.returncode == 0 and result.stdout == b"refused 7\n":
        raise ValueError("HTTPS CA bundle does not match its pin")
    if result.returncode or not REPLY.fullmatch(result.stdout):
        raise ValueError("Witchy refused the mirror identity or route response")
    return result.stdout.decode("ascii")
