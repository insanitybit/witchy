"""Prepare a verified server chain through the scoped Witchy TLS worker."""
from pathlib import Path
import subprocess
import tempfile

from coven_https import private_tls_key
from coven_https_client import CA_FILE_BYTES, trust_path
from coven_staging import read_file
from coven_target import write_new
from coven_worker import check_worker_pin, run_with_lifeline, worker_arguments

BUILD = ("--heap-pages=48", "--slots=4096", "--stack-bytes=65536")
MODULE_BYTES = 131072
CHAIN_BYTES = 65536
VERIFIED = b"verified\n"


def prepare(args, build, host):
    origin = args.origin.encode("utf-8")
    if len(origin) > 269:
        raise ValueError("TLS origin exceeds its size limit")
    with (tempfile.TemporaryDirectory(prefix="coven-tls-identity-") as directory,
          worker_arguments(build, "tls_prepare", BUILD, MODULE_BYTES, "TLS identity",
                           source=args.module, pin=args.module_pin) as module_args):
        source = Path(directory).resolve() / "source"
        source.mkdir(mode=0o700)
        (source / "origin").write_bytes(origin)
        (source / "chain.pem").write_bytes(read_file(args.fullchain.resolve(strict=True), CA_FILE_BYTES))
        (source / "roots.pem").write_bytes(read_file(trust_path(args.ca_file), CA_FILE_BYTES))
        with private_tls_key(args.key) as fd:
            command = (host, *module_args, f"--dir={source}", "--dir-rights=read",
                       "--no-links", "--clock", "--allocator", "--output", f"--output-bytes={len(VERIFIED) + CHAIN_BYTES}",
                       f"--module-bytes={MODULE_BYTES}", f"--fs-bytes={2 * CA_FILE_BYTES + 1024}",
                       "--memory-pages=64", "--fuel=180000000", "--deadline-ms=4000",
                       f"--secret-slot=signer:sign/tls13/server-p384:fd:{fd}")
            try:
                result = run_with_lifeline(subprocess.run, command, stdin=subprocess.DEVNULL,
                                           capture_output=True, pass_fds=(fd,), timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                raise ValueError("Witchy TLS identity worker unavailable") from None
        check_worker_pin(result, "TLS identity")
        chain = result.stdout.removeprefix(VERIFIED)
        if result.returncode or not result.stdout.startswith(VERIFIED) or not 1 <= len(chain) <= CHAIN_BYTES:
            raise ValueError("Witchy refused the TLS certificate chain, origin or signing key")
        write_new(args.out, chain)
