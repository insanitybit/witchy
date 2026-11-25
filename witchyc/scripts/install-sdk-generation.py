#!/usr/bin/python3 -ISB
"""Publish a Lean SDK as a root-owned, never-updated generation (Darwin only).

    /usr/bin/python3 -I -S -B install-sdk-generation.py --source SDK --dry-run
        Validate the source as the invoking user; creates nothing.

    Publication needs root, SUDO_UID/SUDO_GID and exactly `--source .`: the
    invoking user's shell chooses the working directory, so the privileged
    process resolves no caller path and no ancestor.  Root must be handed these
    exact reviewed bytes (a digest-pinned bootstrap; see the task result), never
    a path another user can rewrite.  The interpreter must run with -I -S.

Publication creates fresh inodes under the fixed store

    /Library/WitchySDK/staging/<nonce>            root-only while under construction
    /Library/WitchySDK/generations/<nonce>/sdk    the published read-only SDK
    /Library/WitchySDK/generations/<nonce>/generation.bin   length-framed manifest

Source access.  The source path, every directory listing and every reopen of a
source object happen with the effective uid/gid of the invoking user and a
group list reduced to that one gid; root credentials are restored only for
work on held staging descriptors.  Each object must be owned by that user, be
readable by others (publication never widens read access as judged by mode
bits), carry no BSD flags and NO extended ACL entry, and is checked before it
is opened and again on the held descriptor before a directory is listed or a
byte is cloned.  ACLs are read from held descriptors (a symlink's own ACL
through an O_SYMLINK descriptor); an ACL that cannot be read refuses.  With
owner-only objects and no ACLs, the owner permission class alone decides
access, so the reduced group list cannot change the answer.  The source ACL
metadata may be read before refusal; data of a refused object never is.  No
source inode is adopted, chowned
or modified and no source tool is executed.  A failure removes only the
staging directory this invocation created.

What the manifest attests, and what it does not.  It records ORIGIN AND SEALED
BYTES ONLY: these inodes were created by root from one user's files and sealed
read-only, with their SHA-256.  Content policy is NONE: root has not reviewed
the Lean content and does not assert that it is any particular toolchain.
Extended attributes are cloned with the files and are neither hashed nor
verified.  Device and inode numbers are audit data; device numbers may differ
after a reboot or remount and must not be an authority input.

A future runtime may treat a generation as an immutable input only when ALL of
these hold: it sits at the fixed path /Library/WitchySDK/generations/<nonce>
(never staging/, never an arbitrary uid-0 directory) reached through
descriptor-checked root-owned ancestors; the permanent nonce equals the
directory name; the recorded identities match; AND a separately reviewed
content acceptance plus a closed loader/search universe exist.  Neither the
manifest nor any hash alone permits skipping an SDK scan.  Nothing reads the
manifest yet, and --dry-run or an unprivileged run never writes one.  There is
no destination, adoption or test-bypass option.
"""
import argparse
import contextlib
import ctypes
import errno
import fcntl
import hashlib
import os
import platform
import secrets
import signal
import stat
import struct
import sys
from collections import namedtuple

STORE = '/Library/WitchySDK'
PROTOCOL = b'witchy-sdk-generation-manifest-1'
ISSUER = b'install-sdk-generation policy 2'
ROLE = b'origin-and-sealed-bytes-only'          # not a statement about Lean content
CONTENT_POLICY = b'content-policy=none'
XATTR_POLICY = b'xattrs=cloned-unhashed-unverified'
MANIFEST = 'generation.bin'

MAX_ENTRIES = 1 << 20
MAX_DEPTH = 64
MAX_PATH = 1024
MAX_LINK_HOPS = 40
MAX_MANIFEST = 1 << 28
HEADROOM = 16 << 30         # free bytes that must remain; never lowered for clones
CHUNK = 1 << 20

# <sys/fcntl.h>, <sys/clonefile.h>, <sys/stdio.h>, <sys/acl.h>, <sys/stat.h> of the macOS SDK.
O_SYMLINK = 0x00200000
CLONE_NOFOLLOW = 0x0001
CLONE_NOOWNERCOPY = 0x0002      # CLONE_ACL (0x0004) is deliberately never passed
RENAME_EXCL = 0x00000004
ACL_TYPE_EXTENDED = 0x00000100
ACL_FIRST_ENTRY, ACL_NEXT_ENTRY = 0, -1
ACL_EXTENDED_ALLOW = 1
F_FULLFSYNC = 51
NGROUPS_MAX = 16                # <sys/syslimits.h>
SIGNALS = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)

DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
FILE_FLAGS = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
LINK_FLAGS = os.O_RDONLY | O_SYMLINK | os.O_NONBLOCK | os.O_CLOEXEC     # the symlink itself

_libc = ctypes.CDLL(None, use_errno=True)
_libc.fclonefileat.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]
_libc.renameatx_np.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
_libc.acl_get_fd_np.argtypes = [ctypes.c_int, ctypes.c_int]
_libc.acl_get_fd_np.restype = ctypes.c_void_p
_libc.acl_get_entry.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.POINTER(ctypes.c_void_p)]
_libc.acl_get_tag_type.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_int)]
_libc.acl_init.argtypes = [ctypes.c_int]
_libc.acl_init.restype = ctypes.c_void_p
_libc.acl_set_fd_np.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_int]
_libc.acl_free.argtypes = [ctypes.c_void_p]
_libc.getgroups.argtypes = [ctypes.c_int, ctypes.POINTER(ctypes.c_uint32)]
_libc.fchflags.argtypes = [ctypes.c_int, ctypes.c_uint]


class Refusal(Exception):
    """The installer declines; nothing was published."""


class PublishedUnconfirmed(Exception):
    """The generation IS published under `nonce`, but a later durability or report step failed."""

    def __init__(self, nonce, cause):
        Exception.__init__(self, '%s' % cause)
        self.nonce = nonce


Caller = namedtuple('Caller', 'uid gid')


class Entry(namedtuple('Entry', 'path kind dev ino mode nlink uid gid size payload')):
    """kind is 'd', 'f' or 'l'.  payload: symlink target, or SHA-256 of a sealed file."""
    __slots__ = ()

    @property
    def identity(self):
        return (self.dev, self.ino, self.uid, stat.S_IFMT(self.mode))


def identity(st):
    return (st.st_dev, st.st_ino, st.st_uid, stat.S_IFMT(st.st_mode))


def _errno(call):
    code = ctypes.get_errno()
    return OSError(code, '%s: %s' % (call, os.strerror(code)))


# ---- system calls -----------------------------------------------------------

def fclonefileat(src_fd, dst_dir_fd, name):
    """New inode `name` in dst_dir_fd sharing storage with the held src_fd."""
    if _libc.fclonefileat(src_fd, dst_dir_fd, os.fsencode(name), CLONE_NOFOLLOW | CLONE_NOOWNERCOPY) != 0:
        raise _errno('fclonefileat')


def rename_excl(src_dir_fd, src, dst_dir_fd, dst):
    if _libc.renameatx_np(src_dir_fd, os.fsencode(src), dst_dir_fd, os.fsencode(dst), RENAME_EXCL) != 0:
        raise _errno('renameatx_np')


def acl_tags(fd):
    """Tag of every extended ACL entry on the open object ([] when it has none)."""
    ctypes.set_errno(0)
    acl = _libc.acl_get_fd_np(fd, ACL_TYPE_EXTENDED)
    if not acl:
        if ctypes.get_errno() == errno.ENOENT:
            return []
        raise _errno('acl_get_fd_np')
    try:
        tags, entry, which = [], ctypes.c_void_p(), ACL_FIRST_ENTRY
        while _libc.acl_get_entry(acl, which, ctypes.byref(entry)) == 0:
            tag = ctypes.c_int()
            if _libc.acl_get_tag_type(entry, ctypes.byref(tag)) != 0:
                raise _errno('acl_get_tag_type')
            tags.append(tag.value)
            which = ACL_NEXT_ENTRY
        return tags
    finally:
        _libc.acl_free(acl)


def strip_acl(fd):
    if not acl_tags(fd):
        return
    empty = _libc.acl_init(0)
    if not empty:
        raise _errno('acl_init')
    try:
        if _libc.acl_set_fd_np(fd, empty, ACL_TYPE_EXTENDED) != 0:
            raise _errno('acl_set_fd_np')
    finally:
        _libc.acl_free(empty)


def free_bytes(fd):
    info = os.fstatvfs(fd)
    return info.f_bavail * info.f_frsize


def full_sync(fd):
    os.fsync(fd)
    fcntl.fcntl(fd, F_FULLFSYNC)


def kernel_groups():
    """The credential's group list from the raw getgroups(2).  (os.getgroups on macOS asks the
    directory service about the effective user instead and ignores setgroups.)"""
    groups = (ctypes.c_uint32 * NGROUPS_MAX)()
    count = _libc.getgroups(NGROUPS_MAX, groups)
    if count < 0:
        raise _errno('getgroups')
    return list(groups[:count])


@contextlib.contextmanager
def source_access(caller):
    """Touch the source with the invoking user's credentials, then restore root exactly.

    `caller` is None when the process already is that user.  Otherwise the process must be
    root: the group list becomes the caller's primary gid alone, then egid, then euid.  One
    group is the reduction: root's own groups would add access the caller lacks, and looking up
    the caller's other groups would need account lookups as root for no benefit, since every
    source object must be owner-readable and other-readable anyway.  Real uid stays 0, so the
    caller's processes cannot signal or trace this one.  Root resumes with only its own gid as
    a group, which euid 0 does not need.  An incomplete switch refuses; an incomplete
    restoration aborts the process, because nothing further could be trusted or cleaned up.
    """
    if caller is None:
        yield
        return
    if os.geteuid() != 0:
        raise Refusal('switching to the caller credentials requires root')
    root_gid = os.getegid()
    try:
        with signals_blocked():
            try:
                os.setgroups([caller.gid])
                os.setegid(caller.gid)
                os.seteuid(caller.uid)
                switched = (os.geteuid(), os.getegid(), kernel_groups()) == (caller.uid, caller.gid, [caller.gid])
            except OSError:
                switched = False
        if not switched:
            raise Refusal('could not assume the caller credentials')
        yield
    finally:
        with signals_blocked():
            try:
                os.seteuid(0)
                os.setgroups([root_gid])
                os.setegid(root_gid)
                restored = (os.geteuid(), os.getegid(), kernel_groups()) == (0, root_gid, [root_gid])
            except OSError:
                restored = False
            if not restored:
                os.write(2, b'fatal: root credentials were not restored; aborting without cleanup\n')
                os._exit(70)


@contextlib.contextmanager
def signals_blocked():
    """Defer SIGINT/SIGTERM/SIGHUP so a short critical section is not torn."""
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)


def open_same(dir_fd, name, expected, flags, path):
    """Open `name` without following links and require the expected inode, owner and type."""
    try:
        fd = os.open(name, flags, dir_fd=dir_fd)
    except OSError as error:
        raise Refusal('%s: cannot open without following links (%s)' % (path, os.strerror(error.errno)))
    if identity(os.fstat(fd)) != expected:
        os.close(fd)
        raise Refusal('%s: changed while being opened' % path)
    return fd


def refuse_acl(fd, path):
    """Fail closed: any extended ACL entry, or an ACL that cannot be read, is unsupported."""
    try:
        tags = acl_tags(fd)
    except OSError as error:
        raise Refusal('%s: ACL cannot be read (%s)' % (path, os.strerror(error.errno or 0)))
    if tags:
        raise Refusal('%s: carries an ACL' % path)


def open_source(dir_fd, name, expected, flags, path):
    """open_same for a source object, which must also carry no ACL before it is used."""
    fd = open_same(dir_fd, name, expected, flags, path)
    try:
        refuse_acl(fd, path)
    except BaseException:
        os.close(fd)
        raise
    return fd


def sha256_fd(fd):
    digest, offset = hashlib.sha256(), 0
    while True:
        chunk = os.pread(fd, CHUNK, offset)
        if not chunk:
            return digest.digest()
        digest.update(chunk)
        offset += len(chunk)


# ---- inventory --------------------------------------------------------------

def check_name(name, path):
    try:
        raw = path.encode('utf-8')
    except UnicodeEncodeError:
        raise Refusal('%r: name is not UTF-8' % path)
    if name in ('', '.', '..') or '/' in name or any(byte < 0x20 or byte == 0x7f for byte in raw):
        raise Refusal('%r: unsafe name' % path)
    if len(raw) > MAX_PATH:
        raise Refusal('%r: path too long' % path)


def walk(root_fd, uid, gid=None, sealed=False):
    """Inventory the tree below root_fd through no-follow descriptors.

    Every object must be a directory, regular file or symlink owned by `uid` on
    the root's device with no BSD flags; ownership and type are checked before
    the object is opened and again on the descriptor.  An unsealed (source)
    tree must be readable by others, so publishing it widens no mode bits.
    With `sealed`, require instead the published metadata (group, read-only
    modes, single link, no ACL) and hash file contents.
    """
    root = os.fstat(root_fd)
    entries = []

    def admit(st, path):
        if st.st_uid != uid:
            raise Refusal('%s: not owned by the expected user' % path)
        if st.st_dev != root.st_dev:
            raise Refusal('%s: on another device' % path)
        if st.st_flags:
            raise Refusal('%s: unexpected BSD flags %#x' % (path, st.st_flags))
        need = 0o005 if stat.S_ISDIR(st.st_mode) else 0o004 if stat.S_ISREG(st.st_mode) else 0
        if not sealed and st.st_mode & need != need:
            raise Refusal('%s: not readable by others; publishing it would widen access' % path)
        if sealed and st.st_gid != gid:
            raise Refusal('%s: unexpected group' % path)

    def admit_sealed(fd, st, modes, path):
        if stat.S_IMODE(st.st_mode) not in modes:
            raise Refusal('%s: mode %o is not sealed' % (path, stat.S_IMODE(st.st_mode)))
        refuse_acl(fd, path)

    def record(path, kind, st, payload=b''):
        if len(entries) >= MAX_ENTRIES:
            raise Refusal('more than %d entries' % MAX_ENTRIES)
        size = st.st_size if kind == 'f' else 0
        entries.append(Entry(path, kind, st.st_dev, st.st_ino, st.st_mode, st.st_nlink,
                             st.st_uid, st.st_gid, size, payload))

    def visit(fd, path, depth):
        st = os.fstat(fd)
        if not stat.S_ISDIR(st.st_mode):
            raise Refusal('%s: not a directory' % path)
        admit(st, path)
        if sealed:
            admit_sealed(fd, st, (0o555,), path)
        else:
            refuse_acl(fd, path)        # before the directory is listed
        if depth > MAX_DEPTH:
            raise Refusal('%s: nested too deeply' % path)
        record(path, 'd', st)
        for name in sorted(os.listdir(fd)):
            child = name if path == '.' else path + '/' + name
            check_name(name, child)
            before = os.stat(name, dir_fd=fd, follow_symlinks=False)
            admit(before, child)
            if stat.S_ISDIR(before.st_mode):
                sub = open_same(fd, name, identity(before), DIR_FLAGS, child)
                try:
                    visit(sub, child, depth + 1)
                finally:
                    os.close(sub)
            elif stat.S_ISREG(before.st_mode):
                held = open_same(fd, name, identity(before), FILE_FLAGS, child)
                try:
                    now, digest = os.fstat(held), b''
                    if sealed:
                        if now.st_nlink != 1:
                            raise Refusal('%s: has another hard link' % child)
                        admit_sealed(held, now, (0o444, 0o555), child)
                        digest = sha256_fd(held)
                    else:
                        refuse_acl(held, child)
                    record(child, 'f', now, digest)
                finally:
                    os.close(held)
            elif stat.S_ISLNK(before.st_mode):
                held = open_same(fd, name, identity(before), LINK_FLAGS, child)    # the link, not its target
                try:
                    if sealed:
                        admit_sealed(held, os.fstat(held), (0o555,), child)
                    else:
                        refuse_acl(held, child)
                finally:
                    os.close(held)
                target = os.readlink(name, dir_fd=fd)
                if identity(os.stat(name, dir_fd=fd, follow_symlinks=False)) != identity(before):
                    raise Refusal('%s: changed while being read' % child)
                try:
                    record(child, 'l', before, target.encode('utf-8'))
                except UnicodeEncodeError:
                    raise Refusal('%s: link target is not UTF-8' % child)
            else:
                raise Refusal('%s: not a regular file, directory or symlink' % child)

    visit(root_fd, '.', 0)
    entries.sort(key=lambda entry: entry.path.encode('utf-8'))
    return entries


def check_links(entries):
    """Every symlink must be relative and resolve, inside the tree, to an existing object."""
    by_path = {entry.path: entry for entry in entries}
    for link in (entry for entry in entries if entry.kind == 'l'):
        current = link.path.split('/')[:-1]
        todo, hops = [], 0

        def follow(target):
            text = target.decode('utf-8')
            if not text or text.startswith('/') or len(target) > MAX_PATH or any(b < 0x20 or b == 0x7f for b in target):
                raise Refusal('%s: link target is empty, absolute or unsafe' % link.path)
            todo[:0] = text.split('/')

        follow(link.payload)
        while todo:
            part = todo.pop(0)
            if part in ('', '.'):
                continue
            if part == '..':
                if not current:
                    raise Refusal('%s: link escapes the SDK' % link.path)
                current.pop()
                continue
            current.append(part)
            node = by_path.get('/'.join(current))
            if node is None:
                raise Refusal('%s: link does not resolve inside the SDK' % link.path)
            if node.kind == 'l':
                hops += 1
                if hops > MAX_LINK_HOPS:
                    raise Refusal('%s: cyclic link' % link.path)
                current.pop()
                follow(node.payload)
            elif node.kind == 'f' and any(rest not in ('', '.') for rest in todo):
                raise Refusal('%s: link passes through a file' % link.path)


def scan_source(root_fd, owner):
    """Validated inventory of a plausible Lean SDK owned by `owner`.  Reads no file contents."""
    entries = walk(root_fd, owner)
    check_links(entries)
    by_path = {entry.path: entry for entry in entries}
    lean, library = by_path.get('bin/lean'), by_path.get('lib/lean')
    if lean is None or lean.kind != 'f' or not lean.mode & 0o100 or library is None or library.kind != 'd':
        raise Refusal('source is not a Lean SDK: need executable file bin/lean and directory lib/lean')
    return entries


# ---- construction -----------------------------------------------------------

def require_space(fd, need=HEADROOM):
    if free_bytes(fd) < need:
        raise Refusal('insufficient free space: %d bytes must be free' % need)


# The launch guard is this same 16 GiB floor on /Library, checked before any store directory is
# created.  A clone shares already-allocated extents, so no whole-SDK reservation is added; only the
# copy fallback adds the one file it is about to write.


def seal(fd, ids, mode):
    os.fchown(fd, ids[0], ids[1])
    strip_acl(fd)
    os.fchmod(fd, mode)
    os.fsync(fd)


def copy_file(src_fd, dst_dir_fd, name, size):
    """Fallback when cloning is unsupported: copy from the held descriptor with explicit headroom."""
    require_space(dst_dir_fd, HEADROOM + size)
    before = os.fstat(src_fd)
    out = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=dst_dir_fd)
    try:
        offset = 0
        while True:
            chunk = os.pread(src_fd, CHUNK, offset)
            if not chunk:
                break
            offset += len(chunk)
            if offset > size:
                raise Refusal('%s: source grew while being copied' % name)
            while chunk:
                chunk = chunk[os.write(out, chunk):]
        os.fsync(out)
    finally:
        os.close(out)
    after = os.fstat(src_fd)
    if offset != size or (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
        raise Refusal('%s: source changed while being copied' % name)


def stage(source_fd, entries, dst_fd, ids, caller=None):
    """Recreate the scanned inventory as fresh sealed inodes inside the new directory dst_fd.

    Every source open, stat and readlink runs inside source_access(caller); cloning, copying
    from the already-held descriptor and all destination work run with the process's own
    credentials.  A held descriptor is used only after it matches the validated inode and
    still carries no ACL."""
    children = {}
    for entry in entries[1:]:
        children.setdefault(entry.path.rpartition('/')[0] or '.', []).append(entry)
    done = [0]

    def fill(src, dst, path):
        for entry in children.get(path, ()):
            name = entry.path.rpartition('/')[2]
            if done[0] % 256 == 0:
                require_space(dst)
            done[0] += 1
            if entry.kind == 'd':
                with signals_blocked(), source_access(caller):
                    sub_src = open_source(src, name, entry.identity, DIR_FLAGS, entry.path)
                try:
                    os.mkdir(name, 0o700, dir_fd=dst)
                    sub_dst = os.open(name, DIR_FLAGS, dir_fd=dst)
                    try:
                        fill(sub_src, sub_dst, entry.path)
                        seal(sub_dst, ids, 0o555)
                    finally:
                        os.close(sub_dst)
                finally:
                    os.close(sub_src)
            elif entry.kind == 'f':
                with signals_blocked(), source_access(caller):
                    held = open_source(src, name, entry.identity, FILE_FLAGS, entry.path)
                try:
                    if os.fstat(held).st_size != entry.size:
                        raise Refusal('%s: changed after validation' % entry.path)
                    try:
                        fclonefileat(held, dst, name)
                    except OSError as error:
                        if error.errno not in (errno.EXDEV, errno.ENOTSUP):
                            raise
                        copy_file(held, dst, name, entry.size)
                finally:
                    os.close(held)
                made = os.open(name, FILE_FLAGS, dir_fd=dst)    # read-only: clones never get a writable handle
                try:
                    st = os.fstat(made)
                    if not stat.S_ISREG(st.st_mode) or st.st_nlink != 1 or (st.st_dev, st.st_ino) == (entry.dev, entry.ino):
                        raise Refusal('%s: destination is not a fresh unaliased file' % entry.path)
                    if st.st_flags:     # set on the source after validation and carried by the clone
                        _libc.fchflags(made, 0)     # so that this run's cleanup can remove it
                        raise Refusal('%s: unexpected BSD flags appeared after validation' % entry.path)
                    seal(made, ids, 0o555 if entry.mode & 0o111 else 0o444)
                finally:
                    os.close(made)
            else:
                with signals_blocked(), source_access(caller):
                    os.close(open_source(src, name, entry.identity, LINK_FLAGS, entry.path))
                    now = os.stat(name, dir_fd=src, follow_symlinks=False)
                    target = os.readlink(name, dir_fd=src)
                if identity(now) != entry.identity or os.fsencode(target) != entry.payload:
                    raise Refusal('%s: changed after validation' % entry.path)
                os.symlink(target, name, dir_fd=dst)
                os.chown(name, ids[0], ids[1], dir_fd=dst, follow_symlinks=False)
                os.chmod(name, 0o555, dir_fd=dst, follow_symlinks=False)

    refuse_acl(source_fd, '.')
    fill(source_fd, dst_fd, '.')
    seal(dst_fd, ids, 0o555)


def compare(source, sealed):
    """The sealed tree is exactly the validated source inventory, on inodes the source cannot reach."""
    if [entry.path for entry in source] != [entry.path for entry in sealed]:
        raise Refusal('staged names differ from the validated source')
    source_inodes = {(entry.dev, entry.ino) for entry in source}
    for want, have in zip(source, sealed):
        same = want.kind == have.kind and (have.dev, have.ino) not in source_inodes
        if want.kind == 'f':
            same = same and want.size == have.size and bool(want.mode & 0o111) == bool(have.mode & 0o111)
        elif want.kind == 'l':
            same = same and want.payload == have.payload
        if not same:
            raise Refusal('%s: staged object differs from the validated source' % want.path)


def remove_owned(parent_fd, name, owned):
    """Remove the staging directory this invocation created, and nothing else."""
    try:
        fd = os.open(name, DIR_FLAGS, dir_fd=parent_fd)
    except FileNotFoundError:
        return
    try:
        st = os.fstat(fd)
        if (st.st_dev, st.st_ino) != owned:
            raise Refusal('staging %s is not the directory this run created; left untouched' % name)

        def empty(directory):
            os.fchmod(directory, 0o700)
            for child in os.listdir(directory):
                if stat.S_ISDIR(os.stat(child, dir_fd=directory, follow_symlinks=False).st_mode):
                    sub = os.open(child, DIR_FLAGS, dir_fd=directory)
                    try:
                        empty(sub)
                    finally:
                        os.close(sub)
                    os.rmdir(child, dir_fd=directory)
                else:
                    os.unlink(child, dir_fd=directory)

        empty(fd)
    finally:
        os.close(fd)
    os.rmdir(name, dir_fd=parent_fd)


# ---- manifest ---------------------------------------------------------------

def frame(data):
    """One unit_frame: u64 little-endian length, then the bytes."""
    return struct.pack('<Q', len(data)) + data


ENTRY_RECORD = struct.Struct('<BQQIQIIQ')   # kind, dev, ino, mode, nlink, uid, gid, size


def encode_manifest(nonce, sdk_root, machine, generation, entries):
    """Frames: protocol, issuer, platform, nonce, role, content policy, xattr policy, SDK root
    path, generation (dev, ino), entry count, then per entry (path, fixed record, payload), then
    SHA-256 of all prior bytes.  Payload is the content SHA-256 for a file, the target for a
    symlink, empty for a directory.

    The role and policy frames say what this is: origin and sealed bytes only, content not
    reviewed, extended attributes outside the inventory.  dev/ino values and the trailer (which
    covers them and the nonce) are audit and integrity data, not a content identity; see the
    module docstring for what a runtime must additionally require."""
    parts = [frame(PROTOCOL), frame(ISSUER), frame(b'darwin-' + machine.encode('ascii')), frame(nonce.encode('ascii')),
             frame(ROLE), frame(CONTENT_POLICY), frame(XATTR_POLICY), frame(sdk_root.encode('utf-8')),
             frame(struct.pack('<QQ', *generation)), frame(struct.pack('<Q', len(entries)))]
    for entry in entries:
        record = ENTRY_RECORD.pack(ord(entry.kind), entry.dev, entry.ino, entry.mode, entry.nlink,
                                   entry.uid, entry.gid, entry.size)
        parts += [frame(entry.path.encode('utf-8')), frame(record), frame(entry.payload)]
    body = b''.join(parts)
    return body + frame(hashlib.sha256(body).digest())


def decode_manifest(data):
    """Strict inverse of encode_manifest; ValueError on any deviation."""
    if len(data) > MAX_MANIFEST:
        raise ValueError('manifest too large')
    position = [0]

    def take():
        start = position[0]
        if len(data) - start < 8:
            raise ValueError('truncated frame length')
        (length,) = struct.unpack_from('<Q', data, start)
        if length > len(data) - start - 8:
            raise ValueError('truncated frame')
        position[0] = start + 8 + length
        return data[start + 8:position[0]]

    try:
        if take() != PROTOCOL or take() != ISSUER:
            raise ValueError('unknown protocol or issuer')
        platform_name, nonce = take().decode('ascii'), take().decode('ascii')
        if take() != ROLE or take() != CONTENT_POLICY or take() != XATTR_POLICY:
            raise ValueError('unknown role or policy')
        sdk_root = take().decode('utf-8')
        if len(nonce) != 64 or set(nonce) - set('0123456789abcdef'):
            raise ValueError('bad nonce')
        generation = struct.unpack('<QQ', take())
        (count,) = struct.unpack('<Q', take())
        if count > MAX_ENTRIES:
            raise ValueError('too many entries')
        entries, previous = [], None
        for _ in range(count):
            raw_path, record, payload = take(), take(), take()
            kind, dev, ino, mode, nlink, uid, gid, size = ENTRY_RECORD.unpack(record)
            want = {'d': 0, 'f': 32}.get(chr(kind), len(payload))
            if chr(kind) not in 'dfl' or len(payload) != want or (previous is not None and raw_path <= previous):
                raise ValueError('bad entry')
            previous = raw_path
            entries.append(Entry(raw_path.decode('utf-8'), chr(kind), dev, ino, mode, nlink, uid, gid, size, payload))
        body_end = position[0]
        if take() != hashlib.sha256(data[:body_end]).digest() or position[0] != len(data):
            raise ValueError('bad trailer')
    except struct.error:
        raise ValueError('bad fixed-size frame')
    return {'platform': platform_name, 'nonce': nonce, 'sdk_root': sdk_root, 'generation': generation, 'entries': entries}


def mint_manifest(generation_fd, nonce, sealed, ids):
    """Write the certificate.  Only a real root publication of root-owned inodes may do this."""
    if os.geteuid() != 0 or tuple(ids) != (0, 0):
        raise Refusal('no certificate: minting requires a real root publication')
    here = os.fstat(generation_fd)
    data = encode_manifest(nonce, '%s/generations/%s/sdk' % (STORE, nonce), platform.machine(),
                           (here.st_dev, here.st_ino), sealed)
    fd = os.open(MANIFEST, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=generation_fd)
    try:
        rest = data
        while rest:
            rest = rest[os.write(fd, rest):]
        seal(fd, ids, 0o444)
    finally:
        os.close(fd)
    fd = os.open(MANIFEST, FILE_FLAGS, dir_fd=generation_fd)
    try:
        st, stored = os.fstat(fd), b''
        while len(stored) <= len(data):
            chunk = os.pread(fd, CHUNK, len(stored))
            if not chunk:
                break
            stored += chunk
        if (stored != data or st.st_uid != 0 or st.st_gid != 0 or st.st_nlink != 1
                or stat.S_IMODE(st.st_mode) != 0o444 or acl_tags(fd)):
            raise Refusal('manifest did not read back sealed')
        decode_manifest(stored)
    finally:
        os.close(fd)


# ---- publication ------------------------------------------------------------

def publish(staging_fd, generations_fd, nonce, owned):
    """Atomically move the finished generation into the published namespace; never overwrites.
    Once the rename has happened every later problem is reported as PublishedUnconfirmed."""
    rename_excl(staging_fd, nonce, generations_fd, nonce)
    try:
        st = os.stat(nonce, dir_fd=generations_fd, follow_symlinks=False)
        if (st.st_dev, st.st_ino) != owned:
            raise Refusal('published name is not the directory this run created')
        os.fsync(staging_fd)
        full_sync(generations_fd)
    except (OSError, Refusal) as error:
        raise PublishedUnconfirmed(nonce, error)


def install_into(source_fd, entries, staging_fd, generations_fd, ids, caller=None):
    """Stage, verify, certify and publish one generation under a fresh nonce.

    Any failure before publication removes this run's staging directory only.  The certificate
    step refuses unless the process really is root creating root-owned inodes, so an
    unprivileged caller can never produce a published generation.  Signals are deferred while
    the staging directory is created, while it is cleaned up, and from publication onwards.
    """
    nonce = secrets.token_hex(32)
    generation_fd = owned = None
    made = False
    try:
        with signals_blocked():
            os.mkdir(nonce, 0o700, dir_fd=staging_fd)
            made = True
            generation_fd = os.open(nonce, DIR_FLAGS, dir_fd=staging_fd)
            here = os.fstat(generation_fd)
            owned = (here.st_dev, here.st_ino)
        require_space(generation_fd)
        os.mkdir('sdk', 0o700, dir_fd=generation_fd)
        sdk_fd = os.open('sdk', DIR_FLAGS, dir_fd=generation_fd)
        try:
            stage(source_fd, entries, sdk_fd, ids, caller)
            require_space(generation_fd)
            # Every writable handle is closed; verify the complete tree as it will be published.
            sealed = walk(sdk_fd, ids[0], ids[1], sealed=True)
        finally:
            os.close(sdk_fd)
        compare(entries, sealed)
        require_space(generation_fd)
        mint_manifest(generation_fd, nonce, sealed, ids)
        seal(generation_fd, ids, 0o555)
        if sorted(os.listdir(generation_fd)) != [MANIFEST, 'sdk'] or acl_tags(generation_fd):
            raise Refusal('generation directory is not exactly sdk and %s' % MANIFEST)
        full_sync(generation_fd)
        os.close(generation_fd)
        generation_fd = None
        # Deliberately never unblocked: after the rename the only correct outcomes are the
        # success report or PublishedUnconfirmed, not an "interrupted" exit.
        signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
        publish(staging_fd, generations_fd, nonce, owned)
    except PublishedUnconfirmed:
        raise
    except BaseException:
        with signals_blocked():
            if generation_fd is not None:
                os.close(generation_fd)
            if owned is not None:
                remove_owned(staging_fd, nonce, owned)    # a no-op if the name is already gone
            elif made:
                os.rmdir(nonce, dir_fd=staging_fd)        # created an instant ago, still empty
        raise
    return nonce, sealed


# ---- the protected store (root only) -----------------------------------------

def require_protected(fd, path, exact_mode=None):
    """A root-owned object no other user can modify.  Pre-existing system ancestors may carry
    deny-only ACLs; directories of the store itself must match exactly and carry none."""
    st, tags = os.fstat(fd), acl_tags(fd)
    if st.st_uid != 0 or st.st_mode & 0o022 or ACL_EXTENDED_ALLOW in tags:
        raise Refusal('%s: not root-owned and closed to other writers' % path)
    if exact_mode is not None and (not stat.S_ISDIR(st.st_mode) or st.st_gid != 0
                                   or stat.S_IMODE(st.st_mode) != exact_mode or tags):
        raise Refusal('%s: existing store directory has unexpected metadata; not repaired' % path)


def check_protected_path(path):
    """Every component of an absolute, symlink-free path is root-owned and closed to other writers."""
    parts = [part for part in path.split('/') if part]
    fd, seen = os.open('/', DIR_FLAGS), '/'
    try:
        require_protected(fd, seen)
        for index, part in enumerate(parts):
            flags = FILE_FLAGS if index == len(parts) - 1 else DIR_FLAGS
            try:
                following = os.open(part, flags, dir_fd=fd)
            except OSError as error:
                raise Refusal('%s%s: %s' % (seen, part, os.strerror(error.errno)))
            os.close(fd)
            fd, seen = following, seen + part + '/'
            require_protected(fd, seen.rstrip('/'))
    finally:
        os.close(fd)


def interpreter_refusal():
    """Why this interpreter must not be used for publication, or None.

    Advisory only: it runs inside the interpreter it judges.  Start-up safety comes from the
    administrator naming the system interpreter literally with -I -S; this catches mistakes and
    lets --dry-run show, without privilege, what that name resolves to on this host."""
    try:
        if not sys.executable:
            raise Refusal('interpreter path is unknown')
        parts = [part for part in os.path.abspath(sys.executable).split('/') if part]
        for count in range(1, len(parts) + 1):      # each prefix, so every symlink sits in a checked directory
            check_protected_path(os.path.realpath('/' + '/'.join(parts[:count])))
    except Refusal as refusal:
        return str(refusal)
    return None


def open_library():
    """The protected /Library directory, reached without following links.  Creates nothing."""
    slash = os.open('/', DIR_FLAGS)
    try:
        require_protected(slash, '/')
        library = os.open('Library', DIR_FLAGS, dir_fd=slash)
    finally:
        os.close(slash)
    try:
        require_protected(library, '/Library')
    except BaseException:
        os.close(library)
        raise
    return library


def open_store(library_fd):
    """Open (creating only missing directories) the fixed store; returns (staging_fd, generations_fd)."""
    if os.geteuid() != 0:
        raise Refusal('the store is created and used by root only')

    def ensure(parent, name, mode, path):
        try:
            os.mkdir(name, mode, dir_fd=parent)
            created = True
        except FileExistsError:
            created = False
        try:
            fd = os.open(name, DIR_FLAGS, dir_fd=parent)
        except OSError as error:
            raise Refusal('%s: %s' % (path, os.strerror(error.errno)))
        try:
            if created:
                seal(fd, (0, 0), mode)
                os.fsync(parent)
            require_protected(fd, path, exact_mode=mode)
        except BaseException:
            os.close(fd)
            raise
        return fd

    top = ensure(library_fd, 'WitchySDK', 0o755, STORE)
    try:
        generations = ensure(top, 'generations', 0o755, STORE + '/generations')
        try:
            staging = ensure(top, 'staging', 0o700, STORE + '/staging')
        except BaseException:
            os.close(generations)
            raise
        return staging, generations
    finally:
        os.close(top)


# ---- command ----------------------------------------------------------------

def sudo_caller(environ):
    """The non-root user and primary group that invoked sudo."""
    ids = []
    for name in ('SUDO_UID', 'SUDO_GID'):
        text = environ.get(name, '')
        if not text.isascii() or not text.isdigit() or not 0 < int(text) < 1 << 31:
            raise Refusal('%s must name the non-root invoking user or group; run through sudo' % name)
        ids.append(int(text))
    return Caller(*ids)


def run(arguments):
    parser = argparse.ArgumentParser(description='Publish a Lean SDK as an immutable root-owned generation.')
    parser.add_argument('--source', required=True, help='SDK directory owned by the invoking user')
    parser.add_argument('--dry-run', action='store_true', help='validate the source only; creates nothing')
    options = parser.parse_args(arguments)
    if sys.platform != 'darwin':
        raise Refusal('Darwin only')
    if not (sys.flags.isolated and sys.flags.no_site):
        raise Refusal('start the interpreter with -I -S')
    os.umask(0o077)
    root = os.geteuid() == 0
    interpreter = interpreter_refusal()
    if not options.dry_run:
        if options.source != '.':
            raise Refusal('publication requires exactly --source . (change into the SDK first)')
        if not root:
            raise Refusal('publication requires root (sudo); --dry-run validates without privilege')
        if interpreter:
            raise Refusal('interpreter is not acceptable for publication: ' + interpreter)
    caller = sudo_caller(os.environ) if root else None

    source_fd = None
    try:
        with source_access(caller):     # path traversal, open and the whole scan as the invoking user
            try:
                source_fd = os.open(options.source, DIR_FLAGS)
            except OSError as error:
                raise Refusal('source: %s' % os.strerror(error.errno))
            entries = scan_source(source_fd, caller.uid if root else os.geteuid())
        count = {kind: sum(entry.kind == kind for entry in entries) for kind in 'dfl'}
        total = sum(entry.size for entry in entries)
        summary = '%d directories, %d files, %d symlinks, %d bytes' % (count['d'], count['f'], count['l'], total)
        library_fd = open_library()
        try:
            if options.dry_run:
                print('dry-run: source is acceptable: ' + summary)
                print('dry-run: interpreter: ' + ('NOT acceptable for publication: ' + interpreter if interpreter
                                                  else 'acceptable (%s)' % os.path.realpath(sys.executable)))
                print('dry-run: free space: %d bytes required under /Library, %d available: %s' % (
                    HEADROOM, free_bytes(library_fd),
                    'enough' if free_bytes(library_fd) >= HEADROOM else 'publication would be REFUSED'))
                print('dry-run: nothing was created; no store, generation or certificate exists from this run')
                return
            require_space(library_fd)       # launch guard: before the store is created or touched
            staging_fd, generations_fd = open_store(library_fd)
            try:
                nonce, _ = install_into(source_fd, entries, staging_fd, generations_fd, (0, 0), caller)
            finally:
                os.close(staging_fd)
                os.close(generations_fd)
        finally:
            os.close(library_fd)
    finally:
        if source_fd is not None:
            os.close(source_fd)
    report_publication(nonce, summary)


def report(nonce, stream=None):
    generation = '%s/generations/%s' % (STORE, nonce)
    for line in ('nonce: ' + nonce, 'sdk: %s/sdk' % generation, 'manifest: %s/%s' % (generation, MANIFEST)):
        print(line, file=stream or sys.stdout)


def report_publication(nonce, summary):
    """Keep a published result distinct even if its output pipe has closed."""
    try:
        print('published: ' + summary)
        report(nonce)
        sys.stdout.flush()
    except OSError as error:
        try:
            sys.stdout.close()
        except OSError:
            pass
        raise PublishedUnconfirmed(nonce, error) from error


def main(arguments=None):
    def interrupted(number, _frame):
        raise SystemExit('interrupted by signal %d; nothing was published' % number)
    for number in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, interrupted)
    try:
        run(arguments)
    except PublishedUnconfirmed as published:
        try:
            print('PUBLISHED, but completion reporting or durability is unconfirmed: %s' % published, file=sys.stderr)
            report(published.nonce, sys.stderr)
            sys.stderr.flush()
        except OSError:
            try:
                sys.stderr.close()
            except OSError:
                pass
        return 2
    except Refusal as refusal:
        print('refused: %s' % refusal, file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print('interrupted; nothing was published', file=sys.stderr)
        return 1
    except OSError as error:
        print('failed: %s' % (error.strerror or error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
