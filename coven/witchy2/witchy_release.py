"""The canonical release of a Witchy package: the one byte string its digest is of.

Two formats, told apart by the first line and never read as each other:

  witchy-package 1    exactly one source file (the format before multi-file packages; its
                      bytes, digests and locks are unchanged)
  witchy-package 2    two or more source files

A release is a record (ASCII lines, then an empty line) followed by a body. Its digest is the
SHA-384 of the record followed by the body. A version 2 record lists every file as
`file PATH LENGTH sha384:HEX`, in the order of PATH's bytes, and the body is those files' bytes
in that order with nothing between or after them. A set of files has exactly one release: one
file is always version 1, two or more always version 2, and every reader re-encodes what it
parsed and refuses bytes that differ.

Every reader of a release (the publisher that packs it, the registry that accepts it, the
client that fetches it, the tool that unpacks it for a build) calls `parse`, so each applies
every rule here. This module is shipped byte for byte with Coven and with Grimoire, each of
which pins its SHA-384; docs/multi-file-packages.md in Grimoire is the specification.
"""
import hashlib
import os
import re
import stat
import unicodedata

FORMAT_1 = "witchy-package 1"
FORMAT_2 = "witchy-package 2"
# A release's record and body together (publish.wy and fetch.wy's bound), which also bounds
# each file; a version 2 release holds 2 to MAX_FILES files (the compiler bundles at most 64).
PAYLOAD_BYTES = 1 << 20
FILE_BYTES = PAYLOAD_BYTES
MAX_FILES = 64
MAX_COMPONENTS = 8
MAX_PATH_BYTES = 255
MAX_COMPONENT_BYTES = 64
# The only kind of file a release holds. Nothing else is allowed: every file must be reached
# by the entry's imports, and only a source can be.
SUFFIX = ".wy"
NAME = re.compile(r"[a-z][a-z0-9-]{0,63}")
VERSION = re.compile(r"(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})")
DIGEST = re.compile(r"sha384:[0-9a-f]{96}")
LENGTH = re.compile(r"0|[1-9][0-9]{0,6}")
# Lower-case ASCII only: no two paths differ by case or by Unicode normalization, on any
# volume. No leading dot, no `.` or `..`, no empty component, nothing a terminal interprets.
COMPONENT = re.compile(r"[a-z0-9_]+(?:[.-][a-z0-9_]+)*")
# What the compiler's `trim_end` removes (Unicode White_Space), so `imports` reads a source's
# header as the compiler does.
WHITE_SPACE = "\t\n\x0b\x0c\r \x85\xa0                　"


class ReleaseError(ValueError):
    """A release, or a package directory, that is refused."""


def digest_of(data):
    return "sha384:" + hashlib.sha384(data).hexdigest()


def shown(text):
    """Untrusted text inside a refusal: ASCII, with everything else escaped."""
    return ascii(text if isinstance(text, str) else bytes(text))


def checked_path(path):
    """`path` if it may name a file of a release; ReleaseError says why not."""
    if not isinstance(path, str) or not path:
        raise ReleaseError("a release path is not empty")
    def refuse(why):
        raise ReleaseError(f"release path {shown(path)}: {why}")
    if not path.isascii():
        refuse("only ASCII (so no two paths differ by Unicode normalization)")
    if any(ord(c) < 0x20 or ord(c) == 0x7f for c in path):
        refuse("no control characters")
    if "\\" in path:
        refuse("`/` separates components, and `\\` is in no path")
    if path.startswith("/"):
        refuse("relative, not absolute")
    if len(path) > MAX_PATH_BYTES:
        refuse(f"at most {MAX_PATH_BYTES} bytes")
    parts = path.split("/")
    if len(parts) > MAX_COMPONENTS:
        refuse(f"at most {MAX_COMPONENTS} components")
    for part in parts:
        if part == "":
            refuse("no empty component")
        if part in (".", ".."):
            refuse("no `.` or `..` component")
        if part.startswith("."):
            refuse("no component begins with a dot")
        if part != part.lower():
            refuse("lower case only (so no two paths differ by case)")
        if len(part) > MAX_COMPONENT_BYTES:
            refuse(f"a component is at most {MAX_COMPONENT_BYTES} bytes")
        if not COMPONENT.fullmatch(part):
            refuse("a component is `a-z`, `0-9` and `_`, with single `.` or `-` between them")
    if not parts[-1].endswith(SUFFIX) or parts[-1] == SUFFIX:
        refuse(f"a release holds only `{SUFFIX}` sources")
    return path


def checked_file_set(paths):
    """`paths`, in a release's order, if they may be one release's files."""
    paths = list(paths)
    if not 2 <= len(paths) <= MAX_FILES:
        raise ReleaseError(f"a `{FORMAT_2}` release holds 2 to {MAX_FILES} files, not {len(paths)} (one file is a `{FORMAT_1}` release)")
    for path in paths:
        checked_path(path)
    for before, after in zip(paths, paths[1:]):
        if before.encode("utf-8", "surrogatepass") >= after.encode("utf-8", "surrogatepass"):
            raise ReleaseError(f"release paths are listed once each, in the order of their bytes: {shown(after)} follows {shown(before)}")
    # Implied by the grammar; checked on its own so that it holds whatever the grammar becomes.
    folded = {}
    for path in paths:
        key = unicodedata.normalize("NFC", path).casefold()
        if key in folded:
            raise ReleaseError(f"release paths {shown(folded[key])} and {shown(path)} are the same path under case folding or normalization")
        folded[key] = path
    listed = set(paths)
    for path in paths:
        parts = path.split("/")
        for depth in range(1, len(parts)):
            if "/".join(parts[:depth]) in listed:
                raise ReleaseError(f"release path {shown('/'.join(parts[:depth]))} is both a file and a directory of {shown(path)}")
    return paths


def checked_requires(name, requires):
    if not isinstance(requires, dict):
        raise ReleaseError("a release's requirements are name and exact version")
    for dependency, exact in requires.items():
        if not isinstance(dependency, str) or not NAME.fullmatch(dependency) or not isinstance(exact, str) or not VERSION.fullmatch(exact):
            raise ReleaseError("invalid package requirement")
        if dependency == name:
            raise ReleaseError("invalid duplicate or self dependency")
    return requires


def record(name, version, requires, entry=None, files=None):
    """The canonical record. `files`: None for version 1, else [(path, length, digest)]."""
    lines = [FORMAT_1 if files is None else FORMAT_2, f"name {name}", f"version {version}"]
    lines += [f"requires {dependency} {exact}" for dependency, exact in sorted(requires.items())]
    if files is not None:
        lines.append(f"entry {entry}")
        lines += [f"file {path} {length} {digest}" for path, length, digest in files]
    return ("\n".join(lines) + "\n\n").encode("ascii")


class Release:
    """A parsed release. `files` is [(path, bytes)] in the release's order; a version 1
    release is its one file, named `source.wy` as every reader has always staged it."""

    def __init__(self, format, name, version, requires, entry, files, record_bytes, body):
        self.format, self.name, self.version, self.requires = format, name, version, requires
        self.entry, self.files, self.record, self.body = entry, files, record_bytes, body
        self.digest = digest_of(record_bytes + body)

    @property
    def payload(self):
        return self.record + self.body

    def listing(self):
        """[(path, length, sha384 digest)] of every file."""
        return [(path, len(data), digest_of(data)) for path, data in self.files]


def imports(source):
    """The files and packages a source's header imports, as written: the text of each
    `import "..."` line before the first declaration, read as the compiler reads it."""
    try:
        text = source.decode("utf-8")
    except UnicodeDecodeError:
        raise ReleaseError("a source is UTF-8") from None
    found = []
    for line in text.split("\n"):
        trimmed = line.rstrip(WHITE_SPACE)
        if trimmed.startswith('import "') and len(trimmed) > len('import "') and trimmed.endswith('"'):
            found.append(trimmed[len('import "'):-1])
            continue
        named = trimmed[len("import "):] if trimmed.startswith("import ") else ""
        if named and all(c in "abcdefghijklmnopqrstuvwxyz_-" for c in named):
            continue  # a standard module, by name
        if trimmed and not trimmed.startswith("//") and not trimmed.startswith("from "):
            break
    return found


def resolved_import(importer, written):
    """The release path `import "written"` in the file `importer` means, or None for a
    package imported by name. Resolved by text alone: an unpacked release holds no link."""
    if written.startswith("@"):
        return None
    if written == "" or written.startswith("/"):
        raise ReleaseError(f"{shown(importer)} imports {shown(written)}: a package's file imports a path relative to itself, inside the package")
    parts = importer.split("/")[:-1]
    for part in written.split("/"):
        if part == ".":
            continue
        if part == "..":
            if not parts:
                raise ReleaseError(f"{shown(importer)} imports {shown(written)}, which leaves the package's directory")
            parts.pop()
            continue
        parts.append(part)
    return "/".join(parts)


def check_closure(requires, entry, files):
    """Every file of the release is reached from the entry by imports that stay inside it, and
    nothing else is imported: a release is exactly the files its entry builds from."""
    reached, queue = {entry}, [entry]
    while queue:
        importer = queue.pop()
        for written in imports(files[importer]):
            target = resolved_import(importer, written)
            if target is None:
                if written[1:] not in requires:
                    raise ReleaseError(f"{shown(importer)} imports the package {shown(written)}, which the release does not require")
                continue
            if target not in files:
                raise ReleaseError(f"{shown(importer)} imports {shown(written)}, which is not a file of the release")
            if target not in reached:
                reached.add(target)
                queue.append(target)
    unreached = sorted(set(files) - reached)
    if unreached:
        raise ReleaseError(f"the release holds {shown(unreached[0])}, which nothing in it imports: a release is the files its entry reaches")


def pack(name, version, requires, entry, files):
    """The release of `files` ({path: bytes}) with the given entry: its payload. One file is
    always a version 1 release, whatever it is named; more are version 2, and are parsed back
    before they are returned, so nothing is packed that a reader would refuse."""
    if not isinstance(name, str) or not NAME.fullmatch(name) or not isinstance(version, str) or not VERSION.fullmatch(version):
        raise ReleaseError("invalid package coordinate")
    checked_requires(name, requires)
    if entry not in files:
        raise ReleaseError("the entry is one of the release's files")
    if len(files) == 1:
        payload = record(name, version, requires) + files[entry]
    else:
        ordered = sorted(files, key=lambda path: path.encode("utf-8", "surrogatepass"))
        head = record(name, version, requires, entry, [(path, len(files[path]), digest_of(files[path])) for path in ordered])
        try:
            head.decode("ascii")
        except UnicodeError:
            raise ReleaseError("release paths are ASCII") from None
        payload = head + b"".join(files[path] for path in ordered)
    if len(payload) > PAYLOAD_BYTES:
        raise ReleaseError(f"the package record and source are {len(payload)} bytes, more than a version holds ({PAYLOAD_BYTES})")
    parse_payload(payload, name, version)
    return payload


def split(payload):
    """(record, body): the record ends with the first empty line."""
    end = payload.find(b"\n\n")
    if end < 0:
        raise ReleaseError("incomplete package record")
    return payload[:end + 2], payload[end + 2:]


def parse_payload(payload, name=None, version=None):
    return parse(*split(payload), name, version)


def parse(record_bytes, body, name=None, version=None):
    """The release whose record is exactly `record_bytes` and whose body is `body` (as a
    registry slot or a cache holds them, `package` and `source.wy`). Refused unless the record
    is canonical and, for version 2, the body is exactly the listed files. With `name` and
    `version`, it must be the release of that coordinate."""
    if len(record_bytes) + len(body) > PAYLOAD_BYTES:
        raise ReleaseError(f"a release is at most {PAYLOAD_BYTES} bytes")
    invalid = ReleaseError("invalid canonical package metadata" if name is None else
                           f"the package record is not the record of {name} {version}")
    try:
        text = record_bytes.decode("ascii")
    except UnicodeDecodeError:
        raise invalid from None
    if not text.endswith("\n\n"):
        raise invalid
    lines = text[:-2].split("\n")
    if len(lines) < 3 or lines[0] not in (FORMAT_1, FORMAT_2):
        if lines and lines[0].startswith("witchy-package ") and lines[0] not in (FORMAT_1, FORMAT_2):
            raise ReleaseError(f"release format {shown(lines[0])} is not one this tool reads ({FORMAT_1}, {FORMAT_2}): use a newer tool")
        raise invalid
    if not lines[1].startswith("name ") or not lines[2].startswith("version "):
        raise invalid
    found_name, found_version = lines[1][5:], lines[2][8:]
    if not NAME.fullmatch(found_name) or not VERSION.fullmatch(found_version):
        raise invalid
    if name is not None and (found_name, found_version) != (name, version):
        raise invalid
    requires, at = {}, 3
    while at < len(lines) and lines[at].startswith("requires "):
        fields = lines[at].split(" ")
        if len(fields) != 3 or not NAME.fullmatch(fields[1]) or not VERSION.fullmatch(fields[2]) \
                or fields[1] == found_name or fields[1] in requires:
            raise ReleaseError("invalid package requirement" if len(fields) != 3 or not NAME.fullmatch(fields[1]) or not VERSION.fullmatch(fields[2])
                               else "invalid duplicate or self dependency")
        requires[fields[1]] = fields[2]
        at += 1
    if lines[0] == FORMAT_1:
        # One file: nothing more in the record, and the body is that file, whatever it holds.
        if at != len(lines) or record_bytes != record(found_name, found_version, requires):
            raise invalid
        return Release(1, found_name, found_version, requires, "source.wy", [("source.wy", body)], record_bytes, body)
    if at >= len(lines) or not lines[at].startswith("entry "):
        raise invalid
    entry = lines[at][6:]
    listed = []
    for line in lines[at + 1:]:
        fields = line.split(" ")
        if len(fields) != 4 or fields[0] != "file" or not LENGTH.fullmatch(fields[2]) or not DIGEST.fullmatch(fields[3]):
            raise invalid
        listed.append((fields[1], int(fields[2]), fields[3]))
    checked_file_set([path for path, _, _ in listed])
    if record_bytes != record(found_name, found_version, requires, entry, listed):
        raise invalid
    if entry not in {path for path, _, _ in listed}:
        raise ReleaseError(f"the release's entry {shown(entry)} is not one of its files")
    if "/" in entry:
        raise ReleaseError(f"the release's entry {shown(entry)} is at the top of the package (its directory is the package's)")
    if any(length > FILE_BYTES for _, length, _ in listed) or sum(length for _, length, _ in listed) != len(body):
        raise ReleaseError("the release's body is not exactly the files its record lists")
    files, start = [], 0
    for path, length, digest in listed:
        data = body[start:start + length]
        start += length
        if digest_of(data) != digest:
            raise ReleaseError(f"the release's file {shown(path)} does not have the digest its record lists")
        files.append((path, data))
    check_closure(requires, entry, dict(files))
    return Release(2, found_name, found_version, requires, entry, files, record_bytes, body)


def _open_directory(name, parent=None):
    return os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent)


def read_file(root, path, limit=PAYLOAD_BYTES):
    """The bytes of the file at `path` (components separated by `/`) below the open directory
    `root`: each component opened through its parent's handle without following a link, and
    the file a regular one with a single name, on the directory's own device. Read once."""
    device = os.fstat(root).st_dev
    held, current = [], root
    refused = lambda why: ReleaseError(f"{shown(path)}: {why}")
    try:
        parts = path.split("/")
        for part in parts[:-1]:
            try:
                current = _open_directory(part, current)
            except OSError:
                raise refused("a directory on its way is missing, or is a link or not a directory") from None
            held.append(current)
            if os.fstat(current).st_dev != device:
                raise refused("is on another device than the package")
        try:
            handle = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=current)
        except OSError:
            raise refused("is missing, or is a symbolic link (a package's files are regular files inside it)") from None
        held.append(handle)
        status = os.fstat(handle)
        if not stat.S_ISREG(status.st_mode):
            raise refused("is not a regular file")
        if status.st_nlink != 1:
            raise refused("has more than one name (a hard link): a package's files are its own")
        if status.st_dev != device:
            raise refused("is on another device than the package")
        data = bytearray()
        while len(data) <= limit:
            block = os.read(handle, min(1 << 16, limit + 1 - len(data)))
            if not block:
                break
            data += block
        if len(data) > limit:
            raise refused(f"is larger than a version holds ({limit} bytes)")
        return bytes(data)
    finally:
        for descriptor in held:
            os.close(descriptor)


def read_package(root, entry, excluded=lambda path: False):
    """{path: bytes}: the entry and every file its imports reach, read below the open
    directory `root` (`read_file`). A file is read once, and what was read is what is packed.
    A package of one file may name it anything a directory entry can be; as soon as there
    are more, every path obeys `checked_path`. `excluded(path)` refuses a file by its path
    (a test member)."""
    if "/" in entry or entry in ("", ".", "..") or "\0" in entry:
        raise ReleaseError("the entry is a file of the package's directory")
    files, queue = {entry: read_file(root, entry)}, [entry]
    while queue:
        importer = queue.pop()
        for written in imports(files[importer]):
            target = resolved_import(importer, written)
            if target is None or target in files:
                continue
            checked_path(entry)
            checked_path(target)
            if excluded(target):
                raise ReleaseError(f"{shown(importer)} imports {shown(written)}: a release contains the production source, not a package test member")
            if len(files) >= MAX_FILES:
                raise ReleaseError(f"a release holds at most {MAX_FILES} files")
            files[target] = read_file(root, target)
            if sum(map(len, files.values())) > PAYLOAD_BYTES:
                raise ReleaseError(f"the package's sources are more than a version holds ({PAYLOAD_BYTES} bytes)")
            queue.append(target)
    return files


def extract(release, parent, name):
    """Write a parsed release's files into a new directory `name` of the directory at
    `parent`, and return the new directory's path. Create each directory and file
    exclusively through its parent's handle without following links."""
    if not isinstance(release, Release) or release.format not in (1, 2):
        raise ReleaseError("only a parsed release is unpacked")
    if "/" in name or name in ("", ".", ".."):
        raise ReleaseError("a release is unpacked into a new directory of the given one")
    if release.format == 1:
        if release.entry != "source.wy" or release.files != [("source.wy", release.body)]:
            raise ReleaseError("a single-file release holds exactly its source.wy")
    else:
        checked_file_set([path for path, _ in release.files])
    held = []
    try:
        top = _open_directory(parent)
        held.append(top)
        os.mkdir(name, 0o700, dir_fd=top)
        root = _open_directory(name, top)
        held.append(root)
        made = {(): root}
        for path, data in release.files:
            parts = tuple(path.split("/"))
            for depth in range(1, len(parts)):
                if parts[:depth] not in made:
                    os.mkdir(parts[depth - 1], 0o700, dir_fd=made[parts[:depth - 1]])
                    made[parts[:depth]] = _open_directory(parts[depth - 1], made[parts[:depth - 1]])
                    held.append(made[parts[:depth]])
            handle = os.open(parts[-1], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o400, dir_fd=made[parts[:-1]])
            try:
                written = 0
                while written < len(data):
                    written += os.write(handle, data[written:])
            finally:
                os.close(handle)
    except OSError as error:
        raise ReleaseError(f"cannot unpack the release into a new directory: {error.strerror or error}") from None
    finally:
        for descriptor in held:
            os.close(descriptor)
    return os.path.join(os.fspath(parent), name)
