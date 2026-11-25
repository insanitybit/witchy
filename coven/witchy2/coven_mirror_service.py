"""Launch the pinned Witchy mirror with no compiler or private action channel."""
import os
from pathlib import Path
import signal
import subprocess

from coven_https import notify_ready, private_tls_key, readiness_destination, require_loopback
from coven_https_client import CA_FILE_BYTES
from coven_tls_prepare import CHAIN_BYTES, MODULE_BYTES as CHECK_MODULE_BYTES
from coven_tuf_publish import directory
from coven_worker import check_worker_pin, run_with_lifeline
from witchy_release import DIGEST

MODULE_BYTES = 131072
TLS_VERIFIED = b"verified\n"

# These are the same per-connection budgets as mirror-https.toml.
FLAGS = (
    "--allocator", "--rand", "--no-links", "--data-only", "--dir-rights=read",
    "--instance-per-connection", "--waiting-connections=16",
    "--deadline-ms=none", "--connection-deadline-ms=2000", "--memory-pages=11",
    f"--module-bytes={MODULE_BYTES}", "--fuel=30000000", "--net-bytes=unlimited",
    "--net-bytes-per-connection=2260992", "--fs-bytes=unlimited",
    "--fs-bytes-per-connection=2163200",
)


def stop(signum, _frame):
    raise SystemExit(128 + signum)


def serve(args, host):
    require_loopback(args.listen)
    mirror = Path(os.path.abspath(args.mirror))
    if mirror == Path("/"):
        raise ValueError("the mirror directory cannot be the filesystem root")
    ready = readiness_destination()
    tls_check = (args.tls_ca_file, args.tls_check_module, args.tls_check_pin)
    verify_tls = ready is not None or args.tls_ca_pin is not None or any(value is not None for value in tls_check)
    if verify_tls and not all(value is not None for value in tls_check):
        raise ValueError("mirror readiness requires --tls-ca-file, --tls-check-module and --tls-check-pin")
    if args.tls_ca_pin is not None and not DIGEST.fullmatch(args.tls_ca_pin):
        raise ValueError("--tls-ca-pin requires an independent SHA-384 pin")
    previous = signal.signal(signal.SIGTERM, stop)
    try:
        with directory(mirror) as held, private_tls_key(args.tls_key) as issuer:
            status = os.fstat(held)
            if verify_tls:
                check_identity(host, *tls_check, mirror, status, issuer, ca_pin=args.tls_ca_pin)
                os.lseek(issuer, 0, os.SEEK_SET)
            return run(host, args.module, args.module_pin, mirror, status, args.listen, issuer, ready)
    finally:
        signal.signal(signal.SIGTERM, previous)


def check_identity(host, ca, module, pin, mirror, status, issuer, *, ca_pin=None):
    command = (host, str(module), f"--module-pin={pin}", f"--module-bytes={CHECK_MODULE_BYTES}",
               f"--dir={mirror}", f"--dir-id={status.st_dev}:{status.st_ino}", "--dir-rights=read",
               f"--file={ca.resolve(strict=True)}", "--no-links", "--data-only", "--clock", "--allocator",
               "--output", "--output-bytes=64", "--memory-pages=64", "--fuel=180000000",
               "--deadline-ms=4000", f"--fs-bytes={CA_FILE_BYTES + CHAIN_BYTES + 4096}",
               f"--secret-slot=signer:sign/tls13/server-p384:fd:{issuer}")
    try:
        result = run_with_lifeline(subprocess.run, command, stdin=subprocess.DEVNULL, capture_output=True,
                                   pass_fds=(issuer,), timeout=5)
    except (OSError, subprocess.TimeoutExpired):
        raise ValueError("Witchy mirror TLS check unavailable") from None
    check_worker_pin(result, "mirror TLS check")
    reply = result.stdout
    if result.returncode or not reply.startswith(TLS_VERIFIED) or len(reply) not in (len(TLS_VERIFIED), len(TLS_VERIFIED) + 48):
        raise ValueError("Witchy refused the installed mirror TLS certificate chain, hostname or signing key")
    if ca_pin is not None and reply[len(TLS_VERIFIED):] != bytes.fromhex(ca_pin.removeprefix("sha384:")):
        raise ValueError("mirror TLS CA bundle differs from its independent pin")


def run(host, wasm, pin, mirror, status, listen, issuer, ready):
    read_end, write_end = os.pipe()
    with os.fdopen(read_end, "rb"), os.fdopen(write_end, "wb") as lifetime:
        command = (host, str(wasm), *FLAGS, f"--listen={listen}", f"--dir={mirror}",
                   f"--module-pin={pin}", f"--dir-id={status.st_dev}:{status.st_ino}", f"--lifeline=fd:{read_end}",
                   f"--secret-slot=issuer:sign/tls13/server-p384:fd:{issuer}")
        if ready is not None:
            command += ("--listener-ready-stdout",)
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE if ready is not None else None,
                                   pass_fds=(issuer, read_end), env={})
        try:
            notify_ready(process, ready)
            code = process.wait()
            return 128 - code if code < 0 else code
        finally:
            lifetime.close()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
