"""Check whether filesystem ACLs extend access beyond mode bits."""
from contextlib import contextmanager
import fcntl
import os
import stat
import sys
import time


_LIBC = []


def _libc():
    """The C library, loaded once (finding it probes the file system each time)."""
    if not _LIBC:
        import ctypes, ctypes.util
        libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
        libc.acl_get_fd_np.restype = ctypes.c_void_p
        _LIBC.append(libc)
    return _LIBC[0]


def extended_acl(handle):
    """Whether the open file has an access control list beyond its mode bits, which can let others
    read it whatever `chmod 600` says (review of 2269af1: macOS `chmod +a`). Fails closed: True
    when it cannot tell."""
    import ctypes
    try:
        if sys.platform == "darwin":
            libc = _libc()
            acl = libc.acl_get_fd_np(handle, 0x00000100)  # ACL_TYPE_EXTENDED
            if acl:
                libc.acl_free(ctypes.c_void_p(acl))
                return True
            return ctypes.get_errno() != 2  # ENOENT: no ACL
        if sys.platform.startswith("linux"):
            return any(name in ("system.posix_acl_access", "system.nfs4_acl") for name in os.listxattr(handle))
    except (OSError, AttributeError):
        return True
    return True


def lock_exclusive(fd, seconds):
    """Acquire the caller's open lock descriptor within a monotonic budget."""
    deadline = time.monotonic() + seconds
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return
        except BlockingIOError:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError from None
            time.sleep(min(remaining, 0.05))


@contextmanager
def private_seed(path, description):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        status = os.fstat(fd)
        if (not stat.S_ISREG(status.st_mode) or status.st_nlink != 1 or status.st_uid != os.getuid()
                or status.st_mode & 0o077 or status.st_size != 32 or extended_acl(fd)):
            raise ValueError(f"{description} must be a private 32-byte file owned by this user")
        yield fd, status
    finally:
        os.close(fd)
