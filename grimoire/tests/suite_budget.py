"""Time the complete suite worker, including interpreter shutdown. Report every failure."""
import os
import sys
import time


def process_age():
    """Seconds since OS launch, including Python startup. Both suite platforms expose it."""
    if sys.platform == "darwin":
        import ctypes
        import struct
        # libproc PROC_PIDTBSDINFO: struct proc_bsdinfo ends with timeval seconds/useconds.
        buf = ctypes.create_string_buffer(1024)
        size = ctypes.CDLL("/usr/lib/libproc.dylib").proc_pidinfo(os.getpid(), 3, 0, buf, len(buf))
        if size >= 16:
            seconds, micros = struct.unpack_from("=QQ", buf, size - 16)
            age = time.time() - (seconds + micros / 1e6)
            if 0 <= age < 86400:
                return age
    elif sys.platform.startswith("linux"):
        with open("/proc/self/stat") as source:
            fields = source.read().rsplit(")", 1)[1].split()
        return time.clock_gettime(time.CLOCK_BOOTTIME) - int(fields[19]) / os.sysconf("SC_CLK_TCK")
    return None


def supervise():
    now = time.monotonic()
    age = process_age()
    start = time.monotonic() - age if age is not None else now
    child = os.fork()
    if child == 0:
        return
    _, status = os.waitpid(child, 0)
    elapsed = time.monotonic() - start
    code = os.waitstatus_to_exitcode(status)
    verdict = "HARD MAX MISSED" if elapsed > 5 else "target missed" if elapsed > 2 else "target met"
    if code:
        verdict = "suite failed, " + verdict.replace("target met", "within time limit")
    interval = "launch through worker teardown" if age is not None else "supervisor start through worker teardown"
    print(f"{interval} {elapsed:.3f}s | {verdict} (target 2s, max 5s) | worker status {code}", flush=True)
    sys.exit(128 - code if code < 0 else code or int(elapsed > 5))
