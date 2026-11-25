"""Prepare locally built and independently pinned Wasmtime workers."""
from contextlib import contextmanager
import fcntl
import os
from pathlib import Path
import stat
import tempfile
import threading

from coven_staging import read_file
from witchy_release import digest_of


STAGED_FILE_ENTRY_BYTES = 256  # Wasmtime charges this when publishing a new file.


def materialize_worker(build, program, module, options, limit, label, *, source=None, pin=None):
    if (source is None) != (pin is None):
        raise ValueError(f"supply the {label} module and its independent pin together")
    if source is None:
        build(program, module, options)
    else:
        wasm = read_file(source, limit)
        if digest_of(wasm) != pin:
            raise ValueError(f"{label} module differs from its independent pin")
        module.write_bytes(wasm)


@contextmanager
def worker_module(build, program, options, limit, label, *, source=None, pin=None):
    with tempfile.TemporaryDirectory(prefix="coven-worker-") as directory:
        module = Path(directory) / f"{program}.wasm"
        materialize_worker(build, program, module, options, limit, label, source=source, pin=pin)
        yield module


@contextmanager
def snapshot_worker_arguments(build, program, options, limit, label, *, source=None, pin=None):
    """Stage the worker privately and bind every host invocation to the staged bytes."""
    with worker_module(build, program, options, limit, label, source=source, pin=pin) as module:
        expected = pin or digest_of(read_file(module, limit))
        yield (str(module), f"--module-pin={expected}")


@contextmanager
def worker_arguments(build, program, options, limit, label, *, source=None, pin=None):
    if (source is None) != (pin is None):
        raise ValueError(f"supply the {label} module and its independent pin together")
    if source is not None:
        yield (str(Path(source).absolute()), f"--module-pin={pin}")
    else:
        with worker_module(build, program, options, limit, label) as module:
            yield (str(module),)


def check_worker_pin(result, label):
    if result.returncode and b"witchy: module differs from its independent pin" in result.stderr.splitlines():
        raise ValueError(f"{label} module differs from its independent pin")


def run_with_lifeline(run, command, *, pass_fds=(), **options):
    """Keep the pipe writer in the launcher so Wasmtime stops if the launcher exits."""
    read_end, write_end = os.pipe()
    try:
        return run((*command, f"--lifeline=fd:{read_end}"), env={},
                   pass_fds=(*pass_fds, read_end), **options)
    finally:
        os.close(write_end)
        os.close(read_end)


def follow_launcher():
    descriptor = os.environ.pop("WITCHY_COVEN_LIFELINE_FD", None)
    if descriptor is None:
        return
    try:
        fd = int(descriptor)
        if fd < 3 or str(fd) != descriptor or not stat.S_ISFIFO(os.fstat(fd).st_mode):
            raise ValueError
        flags = fcntl.fcntl(fd, fcntl.F_GETFL)
        if flags & (os.O_ACCMODE | os.O_NONBLOCK) != os.O_RDONLY:
            raise ValueError
        os.set_inheritable(fd, False)
    except (OSError, ValueError, OverflowError):
        raise ValueError("invalid Coven launcher lifeline. Expected a blocking pipe reader.") from None

    def wait():
        try:
            os.read(fd, 1)
        finally:
            # Closing our descriptors also ends the workers that follow this process.
            os._exit(125)

    threading.Thread(target=wait, daemon=True, name="coven-launcher-lifeline").start()
