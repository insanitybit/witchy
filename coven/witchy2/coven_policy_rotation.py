"""Stage individually selected files for a read-only Witchy trust rotation."""
from contextlib import contextmanager
from pathlib import Path
import subprocess
import tempfile

from coven_approval import JSON_LIMIT
from coven_staging import canonical, read_file, read_json
from coven_target import TARGET_BYTES, TRUST_BYTES, write_new
from coven_worker import check_worker_pin, run_with_lifeline, worker_arguments

MODULE_BYTES = 262144
BUILD = ("--heap-pages=256", "--slots=65536", "--stack-bytes=65536")

REFUSALS = {
    b"refused pin\n": "release trust file does not match its pin",
    b"refused revision\n": "a policy rotation requires a later revision for the same registry and origin",
    b"refused target\n": "Witchy release evidence rejected",
    b"refused size\n": "rotated release trust exceeds its size limit",
    b"refused history\n": "rotated release trust exceeds its archive limits",
}


@contextmanager
def rotation_worker(build, host, *, source=None, pin=None):
    with worker_arguments(build, "tuf_rotate_policy", BUILD, MODULE_BYTES, "policy rotation",
                          source=source, pin=pin) as module:
        yield lambda args: rotate(args, module, host)


def stage(source, name, path, limit):
    wire = read_file(path, limit)
    (source / name).write_bytes(wire)
    return len(wire)


def rotate(args, module, host):
    request = canonical({"trust": args.trust_pin, "count": len(args.bundles)}) + b"\n"
    if len(request) > 1024:
        raise ValueError("release trust file does not match its pin")
    with tempfile.TemporaryDirectory(prefix="coven-rotation-input-") as directory:
        source = Path(directory).resolve()
        size = stage(source, "trust.json", args.trust, TRUST_BYTES)
        size += stage(source, "policy.json", args.policy, JSON_LIMIT)
        for index, bundle in enumerate(args.bundles):
            size += stage(source, f"target-{index}", bundle, TARGET_BYTES)
        deadline = min(600000, 10000 + 200 * len(args.bundles))
        fuel = 8000000000 * (1 + len(args.bundles))
        command = (host, *module, f"--dir={source}", "--no-links", "--clock", "--allocator",
                   "--input-stdin", "--input-bytes=1024", "--output", f"--output-bytes={TRUST_BYTES + 512}",
                   f"--fs-bytes={size + 16384}", f"--module-bytes={MODULE_BYTES}", "--memory-pages=400",
                   f"--fuel={fuel}", f"--deadline-ms={deadline}")
        try:
            result = run_with_lifeline(subprocess.run, command, input=request, capture_output=True, timeout=deadline / 1000 + 2)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("Witchy policy rotation unavailable") from None
    check_worker_pin(result, "policy rotation")
    if result.returncode == 0 and result.stdout in REFUSALS:
        raise ValueError(REFUSALS[result.stdout])
    verdict, _, output = result.stdout.partition(b"\n")
    summary, _, trust = output.partition(b"\n")
    if result.returncode or verdict != b"rotated" or not 0 < len(trust) <= TRUST_BYTES:
        raise ValueError("Witchy policy rotation refused")
    receipt = read_json(summary)
    if not isinstance(receipt, dict) or receipt.keys() != {"revision", "archived_targets", "digest"}:
        raise ValueError("invalid Witchy policy rotation result")
    write_new(args.out, trust)
    return receipt
