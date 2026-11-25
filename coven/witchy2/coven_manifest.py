"""Run the Witchy parser for a package publication manifest."""
from io import BytesIO
import os
from pathlib import Path
import re
import subprocess
import tempfile

from coven_staging import read_file
from coven_worker import check_worker_pin, run_with_lifeline
from witchy_release import DIGEST, digest_of

MANIFEST_BYTES = 128 << 10
MODULE_BYTES = 262144
BUILD = ("--heap-pages=512", "--slots=4096", "--stack-bytes=65536")


def installed_arguments(directory):
    module = directory / "read_package_manifest.wasm"
    pin_file = module.with_suffix(".sha384")
    if not os.path.lexists(module) and not os.path.lexists(pin_file):
        return None
    try:
        pin = read_file(pin_file, 104, owner_writes_only=True).decode("ascii")
    except (OSError, ValueError):
        raise ValueError("invalid bundled package manifest worker pin") from None
    if not pin.endswith("\n") or not DIGEST.fullmatch(pin[:-1]):
        raise ValueError("invalid bundled package manifest worker pin")
    return (module, f"--module-pin={pin[:-1]}")


def read_package_manifest(manifest, package_fd, module_args, host, run, expected_pin):
    try:
        wire = read_file(manifest.name, MANIFEST_BYTES, dir_fd=package_fd)
    except ValueError as error:
        if str(error) == f"input exceeds {MANIFEST_BYTES} bytes":
            raise ValueError(f"package manifest must be UTF-8 and at most {MANIFEST_BYTES} bytes") from None
        raise ValueError(f"cannot read package manifest {manifest}: {error}") from None
    except OSError as error:
        raise ValueError(f"cannot read package manifest {manifest}: {error.strerror}") from None
    digest = digest_of(wire)
    if expected_pin is not None and expected_pin != digest:
        raise ValueError("package manifest differs from its expected SHA-384 pin")
    with tempfile.TemporaryDirectory(prefix="coven-manifest-", dir=os.path.realpath("/tmp")) as directory:
        snapshot = Path(directory) / manifest.name
        snapshot.write_bytes(wire)
        return read_publication(manifest, module_args, host, run, digest, input_path=snapshot)


def read_publication(manifest, module_args, host, run, expected_pin, *, input_path=None):
    """Read one manifest in the worker, and decode its bounded field receipt."""
    if len(module_args) == 1:
        (module,) = module_args
        module_args = (module, f"--module-pin={digest_of(read_file(module, MODULE_BYTES))}")
    limit = MANIFEST_BYTES
    command = (host, *module_args, f"--file={input_path if input_path is not None else manifest}", "--no-links", "--allocator", "--output",
               f"--fs-bytes={limit + 1}", "--fs-entries=1", f"--output-bytes={3 * limit}",
               f"--module-bytes={MODULE_BYTES}", "--memory-pages=768", "--fuel=1500000000", "--deadline-ms=5000")
    try:
        result = run_with_lifeline(run, command, stdin=subprocess.DEVNULL, capture_output=True, timeout=7)
    except subprocess.TimeoutExpired:
        raise ValueError("package manifest worker timed out") from None
    check_worker_pin(result, "package manifest reader")
    if result.returncode:
        raise ValueError(f"cannot read package manifest {manifest}: {result.stderr.decode(errors='replace').strip()}")
    errors = {
        b"hash\n": "cannot hash package manifest",
        b"read\n": f"package manifest must be UTF-8 and at most {limit} bytes",
        b"syntax\n": f"{manifest}: invalid TOML 1.0 package manifest",
        b"package\n": f"{manifest} needs a [package] table with name, version and source",
        b"source\n": "[package].source must name the package's source file",
        b"name\n": "invalid package name: [package].name must be a string",
        b"version\n": "invalid version: an exact MAJOR.MINOR.PATCH",
        b"dependencies\n": '[dependencies] is a table of name = "MAJOR.MINOR.PATCH"',
    }
    stream = BytesIO(result.stdout)
    status = stream.readline(16)
    invalid = "invalid package manifest worker result"

    def field(length):
        if not re.fullmatch(rb"(?:0|[1-9][0-9]{0,5})\n", length) or int(length) > limit:
            raise ValueError(invalid)
        expected = int(length)
        value = stream.read(expected)
        if len(value) != expected:
            raise ValueError(invalid)
        return value.decode("utf-8")

    rejected_fields = {
        b"bad-name\n": "invalid package name {value!r}: lowercase letters, digits and '-', starting with a letter",
        b"bad-version\n": "invalid version {value!r}: an exact MAJOR.MINOR.PATCH",
        b"reserved\n": "{value!r} is reserved for the registry's own files",
        b"self-dependency\n": "{value} cannot depend on itself",
    }
    checked = status == b"ok-v2\n"
    if not checked and status != b"ok\n":
        if status in rejected_fields:
            value = field(stream.readline(8))
            if stream.read(1):
                raise ValueError(invalid)
            raise ValueError(rejected_fields[status].format(value=value))
        raise ValueError(errors.get(status, invalid))
    digest = stream.read(48)
    if len(digest) != 48:
        raise ValueError(invalid)
    if expected_pin is not None and "sha384:" + digest.hex() != expected_pin:
        raise ValueError("package manifest differs from its expected SHA-384 pin")

    name, version, source = (field(stream.readline(8)) for _ in range(3))
    requires = {}
    while (length := stream.readline(8)) != b"end\n":
        if not checked and length == b"version\n" and not stream.read(1):
            raise ValueError(errors[length])
        dependency = field(length)
        if dependency in requires:
            raise ValueError(invalid)
        requires[dependency] = field(stream.readline(8))
    if stream.read(1):
        raise ValueError(invalid)
    return name, version, source, requires, checked
