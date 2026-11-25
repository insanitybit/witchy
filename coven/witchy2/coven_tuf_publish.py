"""Hold the publication lock while Witchy prepares and the host installs a mirror generation."""
from contextlib import contextmanager, ExitStack
import fcntl
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import tempfile

from coven_permissions import extended_acl
from coven_staging import read_file
from coven_worker import check_worker_pin, materialize_worker, run_with_lifeline
from witchy_release import DIGEST, digest_of

TIMESTAMP = Path("metadata/timestamp.json")
ROOT_FILENAME = re.compile(r"([1-9][0-9]*)\.root\.json")
METADATA_FILENAME = re.compile(r"[1-9][0-9]{0,15}\.[a-z][a-z0-9-]{0,63}\.json")
TARGET_FILENAME = re.compile(r"[0-9a-f]{96}\.(?:0|[1-9][0-9]{0,8})(?:\.(?:0|[1-9][0-9]{0,8})){2}\.coven")
PACKAGE = re.compile(r"[a-z][a-z0-9-]{0,63}")
DIRECTORY = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
OBJECT_BYTES = 2 << 20
ROOT_BYTES = 512000
TRUST_BYTES = 256 << 10
MODULE_BYTES = 262144
BUILD = ("--heap-pages=256", "--slots=4096", "--stack-bytes=65536")


@contextmanager
def directory(path):
    """Reach a directory by handle without following any symbolic link."""
    held = os.open("/", DIRECTORY)
    try:
        for part in path.parts[1:]:
            next_fd = os.open(part, DIRECTORY, dir_fd=held)
            os.close(held)
            held = next_fd
        yield held
    finally:
        os.close(held)


def check_owner(fd, label):
    status = os.fstat(fd)
    if status.st_uid != os.getuid() or status.st_mode & 0o022 or extended_acl(fd):
        raise ValueError(f"{label} must be owned and writable only by the publication operator")
    return status


@contextmanager
def subdirectory(parent, name, device, *, create=False):
    try:
        child = os.open(name, DIRECTORY, dir_fd=parent)
    except FileNotFoundError:
        if not create:
            raise
        os.mkdir(name, 0o755, dir_fd=parent)
        os.fsync(parent)
        child = os.open(name, DIRECTORY, dir_fd=parent)
    try:
        if check_owner(child, name).st_dev != device:
            raise ValueError(f"{name} is on another device")
        yield child
    finally:
        os.close(child)


def protected_file(parent, name, device):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=parent)
    try:
        status = check_owner(fd, name)
        if status.st_dev != device or not stat.S_ISREG(status.st_mode) or status.st_nlink != 1:
            raise ValueError(f"{name} is not a singly linked mirror file on its directory's device")
        return fd
    except BaseException:
        os.close(fd)
        raise


def check_published_tree(parent, device, depth):
    # An untrusted account cannot change a checked subtree after this scan.
    with os.scandir(parent) as entries:
        for entry in entries:
            if entry.is_dir(follow_symlinks=False):
                if depth == 0:
                    raise ValueError(f"unexpected directory in published tree: {entry.name}")
                with subdirectory(parent, entry.name, device) as child:
                    check_published_tree(child, device, depth - 1)
            else:
                os.close(protected_file(parent, entry.name, device))


def check_baseline(held, device):
    for name, depth in (("metadata", 0), ("targets", 1)):
        with ExitStack() as stack:
            try:
                child = stack.enter_context(subdirectory(held, name, device))
            except FileNotFoundError:
                continue
            check_published_tree(child, device, depth)


def files_in_spool(spool):
    paths = []
    for path in spool.rglob("*"):
        status = path.lstat()
        if stat.S_ISDIR(status.st_mode):
            continue
        relative = path.relative_to(spool)
        parts = relative.parts
        expected = (relative == TIMESTAMP or
                    (len(parts) == 2 and parts[0] == "metadata" and METADATA_FILENAME.fullmatch(parts[1])) or
                    (len(parts) == 3 and parts[0] == "targets" and PACKAGE.fullmatch(parts[1]) and TARGET_FILENAME.fullmatch(parts[2])))
        if not expected or not stat.S_ISREG(status.st_mode) or status.st_nlink != 1:
            raise ValueError("unexpected object in the Witchy publication spool")
        paths.append(relative)
    if TIMESTAMP not in paths:
        raise ValueError("Witchy did not stage a complete generation")
    return paths


class Publication:
    def __init__(self, mirror, device):
        self.mirror = mirror
        self.device = device

    def install(self, path, source):
        self.install_bytes(path, read_file(source, OBJECT_BYTES), replace=path == TIMESTAMP)

    def install_bytes(self, path, data, *, replace=False):
        with ExitStack() as stack:
            parent = self.mirror
            for component in path.parts[:-1]:
                parent = stack.enter_context(subdirectory(parent, component, self.device, create=True))
            if not replace:
                try:
                    installed = protected_file(parent, path.name, self.device)
                except FileNotFoundError:
                    pass
                else:
                    with os.fdopen(installed, "rb") as existing:
                        if existing.read(len(data) + 1) != data:
                            raise ValueError(f"immutable mirror object conflicts: {path}")
                        os.fsync(existing.fileno())
                    os.fsync(parent)
                    return
            temporary = ".coven-" + secrets.token_hex(16)
            fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW | os.O_CLOEXEC,
                         0o644, dir_fd=parent)
            try:
                with os.fdopen(fd, "wb") as stream:
                    stream.write(data)
                    stream.flush()
                    os.fchmod(stream.fileno(), 0o644)
                    os.fsync(stream.fileno())
                os.replace(temporary, path.name, src_dir_fd=parent, dst_dir_fd=parent)
                os.fsync(parent)
            finally:
                try:
                    os.unlink(temporary, dir_fd=parent)
                except FileNotFoundError:
                    pass

    def commit(self, spool):
        def order(path):
            if path.parts[0] == "targets":
                return 0, 0, path
            if root := ROOT_FILENAME.fullmatch(path.name):
                return 2, int(root[1]), path
            return 1, 0, path

        paths = files_in_spool(spool)
        for path in sorted((p for p in paths if p != TIMESTAMP), key=order):
            self.install(path, spool / path)
        self.install(TIMESTAMP, spool / TIMESTAMP)


def prepare(args, module_args, host, work, incoming, published):
    source = work / "source"
    spool = work / "spool"
    source.mkdir(mode=0o700)
    spool.mkdir(mode=0o700)
    (source / "initial-root.json").write_bytes(read_file(args.root, ROOT_BYTES))
    (source / "release-trust.json").write_bytes(read_file(args.trust, TRUST_BYTES))
    pin_file = work / "pins"
    pin_file.write_bytes(bytes.fromhex(args.root_pin[7:]) + bytes.fromhex(args.trust_pin[7:]))
    command = [host, *module_args, "--output", "--allocator", "--clock", "--no-links",
               f"--dir={source}", f"--read-dir-alias=incoming:fd:{incoming}",
               f"--read-dir-alias=published:fd:{published}", f"--write-dir={spool}", f"--input={pin_file}",
               "--input-bytes=97", "--output-bytes=512", "--fs-bytes=268435456", "--fs-entries=65536",
               "--memory-pages=400", f"--module-bytes={MODULE_BYTES}", "--fuel=8000000000", "--deadline-ms=120000"]
    try:
        ran = run_with_lifeline(subprocess.run, command, stdin=subprocess.DEVNULL, capture_output=True,
                               pass_fds=(incoming, published), timeout=125)
    except subprocess.TimeoutExpired:
        raise ValueError("Witchy publication preparation timed out") from None
    check_worker_pin(ran, "TUF publication")
    if ran.returncode:
        detail = ran.stderr.decode("utf-8", errors="replace").strip()[:512]
        raise ValueError(f"Witchy publication preparation stopped: {detail}")
    if refused := re.fullmatch(rb"refused: ([a-z ]+)\n", ran.stdout):
        raise ValueError(refused[1].decode("ascii"))
    fields = re.fullmatch(rb"verified ([1-9][0-9]{0,15}) ([1-9][0-9]{0,15}) (0|[1-9][0-9]{0,9})\n", ran.stdout)
    if not fields:
        raise ValueError("invalid verdict from Witchy publication preparation")
    return spool, dict(zip(("snapshot", "timestamp", "targets"), map(int, fields.groups())))


def publish(args, build, host, *, source=None, pin=None):
    if not DIGEST.fullmatch(args.root_pin) or not DIGEST.fullmatch(args.trust_pin):
        raise ValueError("publication pins must be SHA-384 digests")
    incoming = Path(os.path.abspath(args.incoming))
    mirror = Path(os.path.abspath(args.mirror))
    if incoming.is_relative_to(mirror) or mirror.is_relative_to(incoming):
        raise ValueError("incoming and mirror must be separate directories")
    with tempfile.TemporaryDirectory(prefix="coven-publication-", dir=os.path.realpath("/tmp")) as temporary:
        work = Path(temporary)
        if work.is_relative_to(incoming) or work.is_relative_to(mirror):
            raise ValueError("publication spool overlaps a repository")
        wasm = work / "publish.wasm"
        materialize_worker(build, "tuf_prepare_repository", wasm, BUILD, MODULE_BYTES, "TUF publication",
                           source=source, pin=pin)
        expected = pin or digest_of(read_file(wasm, MODULE_BYTES))
        module_args = (str(wasm), f"--module-pin={expected}")
        with directory(incoming) as read_fd, directory(mirror) as held:
            root = check_owner(held, "mirror")
            other = os.fstat(read_fd)
            if (root.st_dev, root.st_ino) == (other.st_dev, other.st_ino):
                raise ValueError("incoming and mirror must be separate directories")
            fcntl.flock(held, fcntl.LOCK_EX)
            check_baseline(held, root.st_dev)
            spool, result = prepare(args, module_args, host, work, read_fd, held)
            Publication(held, root.st_dev).commit(spool)
            return result
