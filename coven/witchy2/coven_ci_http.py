"""HTTPS client for Coven private CI staging."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from coven_approval import unb64
from coven_ci import CHALLENGE, RECEIPT, STAGE, TOKEN, TOKEN_BYTES, target_origin
from coven_https_client import CA_FILE_BYTES, read_trust_anchors, trust_path
from coven_permissions import private_seed
from coven_staging import canonical, read_file, read_json
from coven_worker import check_worker_pin, materialize_worker, run_with_lifeline, snapshot_worker_arguments
from witchy_release import DIGEST, NAME, PAYLOAD_BYTES, digest_of, parse_payload

MAX_PROOF_INPUT = PAYLOAD_BYTES + TOKEN_BYTES + 1024
SIGNER_GRANT = "sign/ml-dsa-87/coven-ci-request-1-ml-dsa-87"
MODULE_BYTES = 262144
SIGNER_BUILD = ("--heap-pages=32", "--slots=512", "--stack-bytes=65536")
TRANSPORT_BUILD = ("--heap-pages=48", "--slots=4096", "--stack-bytes=65536")
RESPONSE_BYTES = 16384
TRANSPORT_INPUT = PAYLOAD_BYTES + 16385
REFUSED = re.compile(rb"refused ([0-9]{1,3})\n")
TRANSPORT_ERRORS = {
    1: "invalid CI HTTPS request",
    2: "CI HTTPS transport failed",
    3: "invalid CI HTTPS response framing",
    4: "CI HTTPS server authentication or handshake failed",
    5: "invalid CI HTTPS CA bundle",
    6: "invalid private CI challenge response",
    7: "private CI receipt differs from the request",
    8: "CI HTTPS CA bundle does not match its pin",
}


def signer_worker(build, *, source=None, pin=None):
    return snapshot_worker_arguments(build, "ci_sign", SIGNER_BUILD, MODULE_BYTES, "CI runner signer",
                                     source=source, pin=pin)

def runner_signer(key, module_args, host, request, body=b""):
    with private_seed(key, "CI runner key") as (fd, _):
        command = (host, *module_args, "--input-stdin", "--output", "--allocator",
                   f"--input-bytes={MAX_PROOF_INPUT}", "--output-bytes=8192",
                   f"--module-bytes={MODULE_BYTES}", "--memory-pages=48", "--fuel=120000000",
                   "--deadline-ms=3000", f"--secret-slot=signer:{SIGNER_GRANT}:fd:{fd}")
        try:
            result = run_with_lifeline(subprocess.run, command, input=canonical(request) + b"\n" + body,
                                       capture_output=True, pass_fds=(fd,), timeout=4)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("CI runner signer unavailable") from None
    check_worker_pin(result, "CI runner signer")
    if result.returncode or not result.stdout.endswith(b"\n"):
        raise ValueError("CI runner signer stopped")
    encoded = result.stdout[:-1].decode("ascii")
    expected = 2592 if request["mode"] == "public-key" else 4627
    if encoded == "refused" or len(unb64(encoded)) != expected:
        raise ValueError("CI runner signer refused")
    return encoded


def post(host, module_args, anchors, endpoint, request, body=b"", *, ca_pin):
    verified = f"verified {ca_pin}\n".encode("ascii")
    command = (host, *module_args, f"--file={anchors}", f"--net={endpoint}", "--no-links", "--clock", "--rand",
               "--input-stdin", "--output", "--allocator", f"--input-bytes={TRANSPORT_INPUT}",
               f"--output-bytes={RESPONSE_BYTES + len(verified)}", f"--fs-bytes={CA_FILE_BYTES + 1}",
               "--memory-pages=64", f"--module-bytes={MODULE_BYTES}", "--net-bytes=2097152",
               "--fuel=300000000", "--deadline-ms=10000")
    try:
        result = run_with_lifeline(subprocess.run, command, input=canonical(request | {"ca_pin": ca_pin}) + b"\n" + body,
                                   capture_output=True, timeout=12)
    except (OSError, subprocess.TimeoutExpired):
        raise ValueError("CI HTTPS worker unavailable") from None
    if result.returncode:
        check_worker_pin(result, "CI HTTPS transport")
        raise ValueError("CI HTTPS worker stopped")
    refusal = REFUSED.fullmatch(result.stdout)
    if refusal is not None:
        code = int(refusal[1])
        if 400 <= code <= 599:
            raise ValueError(f"private CI service refused {request['route']} (HTTP {code})")
        raise ValueError(TRANSPORT_ERRORS.get(code, "CI HTTPS worker refused"))
    if not result.stdout.startswith(verified) or len(result.stdout) > RESPONSE_BYTES + len(verified):
        raise ValueError("invalid CI HTTPS worker response (install the current transport worker)")
    document = read_json(result.stdout[len(verified):])
    if not isinstance(document, dict):
        raise ValueError("invalid private CI service response")
    return document


def client(args, build_module, host):
    pins = (args.signer_module, args.signer_pin, args.transport_module, args.transport_pin)
    if any(value is not None for value in pins) and any(value is None for value in pins):
        raise ValueError("supply both CI worker paths and their independent pins")
    origin = target_origin(args.origin)
    endpoint = f"{origin.hostname}:{origin.port or 443}"
    with (signer_worker(build_module, source=args.signer_module, pin=args.signer_pin) as signer,
          tempfile.TemporaryDirectory(prefix="coven-ci-client-") as directory):
        root = Path(directory).resolve()
        module = root / "post.wasm"
        materialize_worker(build_module, "ci_post", module, TRANSPORT_BUILD, MODULE_BYTES, "CI HTTPS transport",
                           source=args.transport_module, pin=args.transport_pin)
        anchors = root / "ca.pem"
        trust = read_trust_anchors(trust_path(args.ca_file), args.ca_pin)
        anchors.write_bytes(trust)
        module_pin = args.transport_pin or digest_of(read_file(module, MODULE_BYTES))
        transport = (str(module), f"--module-pin={module_pin}")
        return exchange(args, host, signer, transport, anchors, endpoint, digest_of(trust))


def exchange(args, host, signer, transport, anchors, endpoint, ca_pin):
    token = sys.stdin.buffer.read(TOKEN_BYTES + 2).rstrip(b"\r\n").decode("ascii")
    if len(token) > TOKEN_BYTES or not TOKEN.fullmatch(token):
        raise ValueError("provide one compact CI identity token on standard input")
    if args.action == "stage":
        route = STAGE
        body = read_file(args.bundle, PAYLOAD_BYTES)
        package = parse_payload(body).name
        digest = digest_of(body)
    else:
        route, package, digest, body = RECEIPT, args.package, args.digest, b""
        if not NAME.fullmatch(package) or not DIGEST.fullmatch(digest):
            raise ValueError("invalid CI receipt coordinate")
    challenge = post(host, transport, anchors, endpoint, {
        "origin": args.origin, "route": CHALLENGE, "body_size": 0,
    }, ca_pin=ca_pin)["nonce"]
    request = {
        "origin": args.origin, "route": route, "token": token, "nonce": challenge,
        "package": package, "digest": digest, "body_size": len(body),
    }
    proof = runner_signer(args.key, signer, host, request | {"mode": "sign"}, body)
    return post(host, transport, anchors, endpoint, request | {"proof": proof}, body, ca_pin=ca_pin)
