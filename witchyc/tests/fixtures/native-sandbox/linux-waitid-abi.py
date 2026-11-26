"""Exercise the native Rust waitid and stat wrappers against Linux libc."""

import ctypes
import os
from pathlib import Path
import signal
import sys
import tempfile


def check_running_child(probe):
    read_end, write_end = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(write_end)
        os.read(read_end, 1)
        os._exit(0)
    os.close(read_end)
    try:
        if probe.probe_running_child(child) != 0:
            raise SystemExit("waitid reported an event for a running child")

        previous_handler = signal.signal(signal.SIGALRM, lambda *_: None)
        try:
            signal.siginterrupt(signal.SIGALRM, True)
            signal.setitimer(signal.ITIMER_REAL, 0.05, 0.05)
            if probe.probe_interrupted_wait(child) != 0:
                raise SystemExit("interrupted waitid did not return EINTR")
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous_handler)
    finally:
        os.close(write_end)
        os.waitpid(child, 0)


def main():
    probe = ctypes.CDLL(str(Path(sys.argv[1]).resolve()))
    probe.probe.argtypes = []
    probe.probe_running_child.argtypes = [ctypes.c_uint32]
    probe.probe_interrupted_wait.argtypes = [ctypes.c_uint32]
    probe.probe_metadata.argtypes = [
        ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_uint64,
        ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_int64,
    ]
    probe.probe_metadata_errors.argtypes = [ctypes.c_int]
    wait_result = probe.probe()
    if wait_result != 0:
        raise SystemExit(f"waitid probe failed with result {wait_result}")

    check_running_child(probe)

    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "sample"
        path.write_bytes(b"abc")
        directory_fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            error_result = probe.probe_metadata_errors(directory_fd)
            if error_result != 0:
                raise SystemExit(f"metadata rejection probe failed with result {error_result}")
            with path.open("rb") as sample:
                file_fd = sample.fileno()
                stat = os.fstat(file_fd)
                result = probe.probe_metadata(
                    directory_fd, file_fd, stat.st_dev, stat.st_ino,
                    stat.st_mode, stat.st_nlink, stat.st_uid, stat.st_size,
                )
        finally:
            os.close(directory_fd)
    if result != 0:
        raise SystemExit(f"metadata probe failed with result {result}")
    print("waitid, all six metadata fields, and metadata rejection passed")


if __name__ == "__main__":
    main()
