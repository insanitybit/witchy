#!/usr/bin/env python3
"""Install the pre-release Witchy tools offline into one prefix.

    python3 install.py --witchy ../witchyc --coven ../coven [--prefix ~/.local/witchy]
    python3 install.py --make-bundle FILE --compiler-binary FILE --host-binary FILE --coven ../coven
    python3 install.py --from-bundle FILE --bundle-pin sha384:DIGEST [--prefix ~/.local/witchy]
    python3 install.py --verify-bundle FILE --bundle-pin sha384:DIGEST [--prefix ~/.local/witchy]
    python3 install.py --check-only [--prefix ~/.local/witchy]

Builds `witchyc` and `witchy-wasmtime-host` with MBX (`--locked --offline`: nothing is
downloaded), then installs into the prefix:

    bin/witchyc, bin/witchy-wasmtime-host, and beside the engine AWS-LC's FIPS crypto library
        (macOS builds it as a dylib; the engine finds it at `@executable_path` and nowhere else)
    libexec/grimoire/  grimoire and every Python module it loads
    libexec/coven/     coven, every Python module it loads, and its Witchy sources
    bin/grimoire, bin/coven: links to those

and checks the result: the engine's library resolves beside it (`otool -L`, no DYLD_ variable),
and Grimoire compiles and runs a proved slice access, refuses a false postcondition, and
builds a library in a temporary directory. The self-check needs a proof toolchain
(`WITCHY_WASM_READY_BINDING` or `WITCHY_WASM_LEAN_ROOT`, docs/coven-http.md).

Which Python modules a tool loads is read from its source (`companions`), not listed here: a
companion added later is installed without a change to this file, and one the source names that
is not there stops the install. The prefix is under the home directory (or the temporary
directory), never a system directory; nothing is written through a symbolic link, and an
existing link inside the prefix where a file or directory is written stops the install.

A prebuilt bundle is a platform-specific, bounded ZIP with an independent SHA-384 pin.
The receiving machine runs a trusted copy of this installer without a checkout, Rust, or MBX.
A proof checker is provisioned separately.

Python 3.11+ standard library only."""

import argparse
import ast
import errno
from hashlib import sha384
from io import BytesIO
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile

HERE = Path(__file__).resolve().parent


class InstallError(Exception):
    pass


def companions(entry):
    """The Python files beside `entry` that it loads, and those they load, in a stable order.
    A file is loaded by an import of its module name or by naming its file (`shipped_module(
    "witchy_compiler.py", ...)`, a helper's name with `.py` added). An import of a module that
    is neither the standard library's nor beside the entry is refused, and so is a `NAME.py`
    the source names that is not beside it: a tool installed without it would fail when that
    path is taken, perhaps long after the install."""
    here = entry.parent
    found, queue = {}, [entry]
    while queue:
        source = queue.pop()
        try:
            tree = ast.parse(source.read_bytes(), str(source))
        except SyntaxError as error:
            raise InstallError(f"{source}: not Python ({error.msg})") from None
        imported, named = set(), set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported |= {alias.name.partition(".")[0] for alias in node.names}
            elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                imported.add(node.module.partition(".")[0])
            elif isinstance(node, ast.ImportFrom):
                raise InstallError(f"{source}: a relative import; a tool's companions are loaded beside it by name")
            elif isinstance(node, ast.Constant) and isinstance(node.value, str):
                named.add(node.value)
        wanted = set()
        for module in imported:
            if (here / f"{module}.py").is_file():
                wanted.add(f"{module}.py")
            elif module not in sys.stdlib_module_names:
                raise InstallError(f"{source}: imports {module}, which is neither the standard library's nor beside it")
        for text in named:
            if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*\.py", text):
                if not (here / text).is_file():
                    raise InstallError(f"{source}: names {text}, which is not beside it")
                wanted.add(text)
            elif re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", text) and (here / f"{text}.py").is_file():
                wanted.add(f"{text}.py")
        for name in sorted(wanted):
            if name != entry.name and name not in found:
                found[name] = here / name
                queue.append(here / name)
    return [found[name] for name in sorted(found)]


def tool_files(entry, data=()):
    """What one tool's directory holds: (relative path, source path, mode) for the entry, its
    companions, and each data file (`data`: glob patterns beside the entry)."""
    files = [(entry.name, entry, 0o755)] + [(path.name, path, 0o644) for path in companions(entry)]
    for pattern in data:
        files += [(str(path.relative_to(entry.parent)), path, 0o644)
                  for path in sorted(entry.parent.glob(pattern)) if path.is_file() and not path.is_symlink()]
    return files


# Where a prefix may be: under the home directory or the temporary directory, as the user who
# runs this. Anything else (/usr/local, /opt, /, a mounted volume) is refused, not asked about.
def checked_prefix(text, *, keep_leaf=False):
    if os.geteuid() == 0:
        raise InstallError("not as root: the tools are installed for one user, under their home directory")
    prefix = Path(os.path.expanduser(text))
    if not prefix.is_absolute():
        raise InstallError(f"--prefix {text}: an absolute path (or one that begins with ~)")
    if keep_leaf and prefix.name not in ("", ".."):
        resolved = Path(os.path.realpath(prefix.parent)) / prefix.name
    else:
        resolved = Path(os.path.realpath(prefix))
    # Fixed roots, not TMPDIR's word: the home directory, /tmp, and macOS's per-user temporary one.
    roots = [Path(os.path.realpath(Path.home())), Path(os.path.realpath("/tmp"))]
    if "CS_DARWIN_USER_TEMP_DIR" in getattr(os, "confstr_names", {}):
        roots.append(Path(os.path.realpath(os.confstr("CS_DARWIN_USER_TEMP_DIR"))))
    if not any(resolved != root and resolved.is_relative_to(root) for root in roots):
        raise InstallError(f"--prefix {text}: is {resolved}, which is not inside the home directory {roots[0]} (or the temporary directory); "
                           "the tools are never installed into a system directory")
    return resolved


class Prefix:
    """The prefix, held by a descriptor; every directory and file below it is reached one
    component at a time with no link followed, so nothing is written outside it."""

    FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | getattr(os, "O_CLOEXEC", 0)

    def __init__(self, path, *, check_owner=True, create=True):
        """`path` is resolved (`checked_prefix`): it is walked from `/` one component at a time,
        made where missing, and no link on the way is followed, so a link put in its place
        since it was checked stops the install rather than leads elsewhere."""
        self.path = path
        self.create = create
        held = os.open("/", self.FLAGS)
        walked = Path("/")
        try:
            for part in path.parts[1:]:
                walked = walked / part
                if self.create:
                    try:
                        os.mkdir(part, 0o755, dir_fd=held)
                    except FileExistsError:
                        pass
                try:
                    inner = os.open(part, self.FLAGS, dir_fd=held)
                except OSError:
                    raise InstallError(f"--prefix {path}: {walked} is a symbolic link or not a directory") from None
                os.close(held)
                held = inner
            if check_owner:
                self.check_mode(held, path)
        except BaseException:
            os.close(held)
            raise
        self.fd = held

    @staticmethod
    def check_mode(fd, shown):
        status = os.fstat(fd)
        if status.st_uid != os.geteuid() or status.st_mode & 0o022:
            raise InstallError(f"{shown}: not this user's, or writable by others (the engine's directory must not be: its crypto library is found there)")

    def directory(self, relative):
        """The descriptor of `relative`, walked without following a link."""
        held = os.dup(self.fd)
        walked = self.path
        try:
            for part in Path(relative).parts:
                walked = walked / part
                if self.create:
                    try:
                        os.mkdir(part, 0o755, dir_fd=held)
                    except FileExistsError:
                        pass
                try:
                    inner = os.open(part, self.FLAGS, dir_fd=held)
                except OSError:
                    raise InstallError(f"{walked}: is a symbolic link or not a directory; nothing is written through it") from None
                os.close(held)
                held = inner
                self.check_mode(held, walked)
            return held
        except BaseException:
            os.close(held)
            raise

    def write(self, relative, data, mode):
        """`data` at `relative`, whole or not at all: written to a new name in the same
        directory, then renamed over what was there (a link there is replaced, never followed)."""
        relative = Path(relative)
        directory = self.directory(relative.parent)
        temporary = f".{relative.name}.installing-{os.getpid()}"
        try:
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | getattr(os, "O_CLOEXEC", 0), 0o600, dir_fd=directory)
            try:
                view = memoryview(data)
                while view:
                    view = view[os.write(fd, view):]
                os.fchmod(fd, mode)
                os.fsync(fd)
            finally:
                os.close(fd)
            existing = self.kind(directory, relative.name)
            if existing is not None and not (stat.S_ISREG(existing) or stat.S_ISLNK(existing)):
                raise InstallError(f"{self.path / relative}: is a directory or something else; nothing replaces it")
            os.replace(temporary, relative.name, src_dir_fd=directory, dst_dir_fd=directory)
        except BaseException:
            try:
                os.unlink(temporary, dir_fd=directory)
            except OSError:
                pass
            raise
        finally:
            os.close(directory)

    def link(self, relative, target):
        """A symbolic link at `relative` to `target`, a path relative to the link's directory
        that stays inside the prefix."""
        relative = Path(relative)
        if Path(target).is_absolute() or not (self.path / relative.parent / target).resolve().is_relative_to(self.path):
            raise InstallError(f"{relative} -> {target}: a link the installer makes stays inside the prefix")
        directory = self.directory(relative.parent)
        temporary = f".{relative.name}.installing-{os.getpid()}"
        try:
            os.symlink(target, temporary, dir_fd=directory)
            existing = self.kind(directory, relative.name)
            if existing is not None and not (stat.S_ISREG(existing) or stat.S_ISLNK(existing)):
                raise InstallError(f"{self.path / relative}: is a directory or something else; nothing replaces it")
            os.replace(temporary, relative.name, src_dir_fd=directory, dst_dir_fd=directory)
        except BaseException:
            try:
                os.unlink(temporary, dir_fd=directory)
            except OSError:
                pass
            raise
        finally:
            os.close(directory)

    @staticmethod
    def kind(directory, name):
        try:
            return os.stat(name, dir_fd=directory, follow_symlinks=False).st_mode
        except FileNotFoundError:
            return None


GRIMOIRE_DATA = ("coven.css", "glamour-browser.mjs", "glamour-worker.mjs", "glamour-frame.html")


# Preserve the guest import paths and the package list's vendor layout.
COVEN_DATA = ("*.wy", "*.policy", "*.packages", "ci/*.wy", "review/*.wy", "tuf/*.wy",
              "mirror/identity.wy", "mirror/paths.wy", "mirror/routes.wy", "mirror/web_*.wy",
              "tls13/*.wy",
              "vendor/sqlite/sqlite.wy", "vendor/toml/toml.wy",
              "vendor/oidc/oidc.wy", "vendor/oidc/jwk_fields.wy", "vendor/oidc/rs256_keys.wy", "vendor/oidc/compact.wy",
              "vendor/webauthn/webauthn.wy", "vendor/webauthn/cose.wy",
              "web/render.wy", "web/render.grants.toml", "web/browser/filter.wy", "web/browser/filter.grants.toml")


def script_files(grimoire, coven):
    for name, entry, data in (("grimoire", grimoire, GRIMOIRE_DATA), ("coven", coven, COVEN_DATA)):
        for filename, source, mode in tool_files(entry, data):
            yield Path("libexec") / name / filename, source, mode


TOOL_LINKS = {name: f"../libexec/{name}/{name}" for name in ("grimoire", "coven")}


def link_tools(prefix):
    for name, target in TOOL_LINKS.items():
        prefix.link(Path("bin") / name, target)


def install_scripts(prefix, grimoire, coven):
    """Install both launchers, their companions and the guest sources."""
    installed = []
    for path, source, mode in script_files(grimoire, coven):
        prefix.write(path, source.read_bytes(), mode)
        installed.append(path)
    link_tools(prefix)
    return installed


BUNDLE_LIMIT = 128 << 20
BUNDLE_FILES = 1024
BUNDLE_ID = f"witchy-client-v1 {sys.platform} {os.uname().machine}".encode("ascii")
BUNDLE_EXECUTABLES = frozenset(("bin/witchyc", "bin/witchy-wasmtime-host",
                               "libexec/grimoire/grimoire", "libexec/coven/coven"))
CRYPTO_MEMBER = r"bin/libaws_lc_fips_[a-z0-9_]+_crypto\.dylib"
SCRIPT_MEMBER = r"libexec/(?:grimoire|coven)/(?:[a-z0-9_][a-z0-9_.-]*/)*[a-z0-9_][a-z0-9_.-]*"
TUF_BUNDLE_DIR = "libexec/coven/tuf-workers/"
TUF_MODULES = ("tuf_fetch_select", "tuf_get", "tuf_get_loopback", "tuf_extract")
WORKER_MODULE_LIMIT = 262144
MANIFEST_MODULE = "libexec/coven/read_package_manifest.wasm"
MANIFEST_PIN = "libexec/coven/read_package_manifest.sha384"


def bundle_members(archive):
    members = archive.infolist()
    if archive.comment != BUNDLE_ID:
        raise InstallError("client bundle has a different format or target platform")
    if len(members) > BUNDLE_FILES or sum(member.file_size for member in members) > BUNDLE_LIMIT:
        raise InstallError("client bundle exceeds its install limits")
    names = set()
    crypto = 0
    for member in members:
        name = member.filename
        if (member.orig_filename != name or name in names or member.compress_type != zipfile.ZIP_STORED
                or member.file_size != member.compress_size
                or stat.S_IFMT(member.external_attr >> 16) not in (0, stat.S_IFREG)):
            raise InstallError("invalid or repeated client bundle member")
        if re.fullmatch(CRYPTO_MEMBER, name):
            crypto += 1
        elif name not in BUNDLE_EXECUTABLES and not re.fullmatch(SCRIPT_MEMBER, name):
            raise InstallError("unexpected client bundle member")
        names.add(name)
    if not BUNDLE_EXECUTABLES <= names or crypto != int(sys.platform == "darwin"):
        raise InstallError("client bundle is missing tools or has the wrong crypto library set")
    bundled_workers = {name.removeprefix(TUF_BUNDLE_DIR) for name in names if name.startswith(TUF_BUNDLE_DIR)}
    if bundled_workers and bundled_workers != {"pins.json", *(f"{name}.wasm" for name in TUF_MODULES)}:
        raise InstallError("client bundle has an incomplete TUF worker set")
    if (MANIFEST_MODULE in names) != (MANIFEST_PIN in names):
        raise InstallError("client bundle has an incomplete package manifest worker")
    if any(str(parent) in names for name in names for parent in Path(name).parents):
        raise InstallError("conflicting paths in client bundle")
    return members


def bundle_files(path, pin):
    if re.fullmatch(r"sha384:[0-9a-f]{96}", pin) is None:
        raise InstallError("--bundle-pin must be sha384: followed by 96 lowercase hex digits")
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > BUNDLE_LIMIT:
            raise InstallError("client bundle must be a regular file within its byte limit")
        wire = source.read(BUNDLE_LIMIT + 1)
    if len(wire) > BUNDLE_LIMIT:
        raise InstallError("client bundle exceeds its byte limit")
    if "sha384:" + sha384(wire).hexdigest() != pin:
        raise InstallError("client bundle does not match its independent pin")
    try:
        with zipfile.ZipFile(BytesIO(wire)) as archive:
            return [(member.filename, archive.read(member)) for member in bundle_members(archive)]
    except (zipfile.BadZipFile, RuntimeError, UnicodeError) as error:
        raise InstallError(f"invalid client bundle: {error}") from None


def verify_bundle(files, prefix_path):
    """Compare the exact installed tree with the pinned bundle without running it."""
    prefix = Prefix(prefix_path, create=False)
    try:
        verify_prefix(files, prefix)
    finally:
        os.close(prefix.fd)


def verify_prefix(files, prefix):
    entries = {}
    for path in [*(Path(name) for name, _ in files), *(Path("bin") / name for name in TOOL_LINKS)]:
        while path != Path("."):
            entries.setdefault(path.parent, set()).add(path.name)
            path = path.parent
    for relative, allowed in entries.items():
        directory = prefix.directory(relative)
        try:
            with os.scandir(directory) as children:
                for child in children:
                    if child.name not in allowed:
                        raise InstallError(f"{prefix.path / relative / child.name}: not in the pinned client bundle")
        finally:
            os.close(directory)

    for name, expected in files:
        relative = Path(name)
        parent = prefix.directory(relative.parent)
        try:
            fd = os.open(relative.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
            with os.fdopen(fd, "rb") as source:
                info = os.fstat(source.fileno())
                mode = 0o755 if name in BUNDLE_EXECUTABLES else 0o644
                if (not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) != mode or info.st_nlink != 1
                        or info.st_uid != os.geteuid() or info.st_size != len(expected)
                        or source.read(len(expected) + 1) != expected):
                    raise InstallError(f"{prefix.path / name}: differs from the pinned client bundle")
        finally:
            os.close(parent)
    binary = prefix.directory("bin")
    try:
        for name, target in TOOL_LINKS.items():
            if os.readlink(name, dir_fd=binary) != target:
                raise InstallError(f"{prefix.path / 'bin' / name}: differs from the client bundle entry link")
    finally:
        os.close(binary)


def worker_bytes(path, label):
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as module:
        info = os.fstat(module.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > WORKER_MODULE_LIMIT:
            raise InstallError(f"{path}: {label} worker must be a bounded regular file")
        body = module.read(WORKER_MODULE_LIMIT + 1)
    if len(body) > WORKER_MODULE_LIMIT or not body.startswith(b"\0asm\x01\0\0\0"):
        raise InstallError(f"{path}: invalid {label} worker module")
    return body


def bundled_workers(folder):
    workers = {}
    pins = {}
    for name in TUF_MODULES:
        filename = f"{name}.wasm"
        body = worker_bytes(folder / filename, "TUF")
        workers[filename] = body
        pins[name] = "sha384:" + sha384(body).hexdigest()
    workers["pins.json"] = (json.dumps(pins, sort_keys=True, separators=(",", ":")) + "\n").encode()
    return workers


def write_bundle(path, compiler, host, grimoire, coven, tuf_workers=None, manifest_worker=None):
    files = [(Path("bin/witchyc"), compiler, 0o755), (Path("bin/witchy-wasmtime-host"), host, 0o755)]
    library = crypto_library(host, host.parent)
    if library is not None:
        files.append((Path("bin") / library.name, library, 0o644))
    files.extend(script_files(grimoire, coven))
    workers = ({TUF_BUNDLE_DIR + name: data for name, data in bundled_workers(tuf_workers).items()}
               if tuf_workers is not None else {})
    if manifest_worker is not None:
        body = worker_bytes(manifest_worker, "package manifest")
        workers[MANIFEST_MODULE] = body
        workers[MANIFEST_PIN] = f"sha384:{sha384(body).hexdigest()}\n".encode("ascii")
    size = sum(source.stat().st_size for _, source, _ in files) + sum(map(len, workers.values()))
    if len(files) + len(workers) > BUNDLE_FILES or size > BUNDLE_LIMIT:
        raise InstallError("client bundle exceeds its install limits")
    with tempfile.TemporaryFile() as stream:
        with zipfile.ZipFile(stream, "w") as archive:
            archive.comment = BUNDLE_ID
            for name, source, _ in files:
                archive.writestr(zipfile.ZipInfo(name.as_posix()), source.read_bytes())
            for name, data in workers.items():
                archive.writestr(zipfile.ZipInfo(name), data)
        stream.seek(0)
        wire = stream.read(BUNDLE_LIMIT + 1)
    if len(wire) > BUNDLE_LIMIT:
        raise InstallError("client bundle exceeds its byte limit")
    pin = "sha384:" + sha384(wire).hexdigest()
    # Check the same inventory a consumer will accept before publishing it.
    with zipfile.ZipFile(BytesIO(wire)) as archive:
        bundle_members(archive)
    with path.open("xb") as output:
        output.write(wire)
    return pin


def publish_new_prefix(parent_fd, staged, prefix_path):
    """Make the checked tree visible with one rename that cannot overwrite a prefix."""
    import ctypes

    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform == "darwin":
        symbol, flags = "renameatx_np", 0x4  # RENAME_EXCL
    elif sys.platform == "linux":
        symbol, flags = "renameat2", 1  # RENAME_NOREPLACE
    else:
        raise InstallError("atomic bundle installation requires macOS or Linux")
    rename = getattr(libc, symbol, None)
    if rename is None:
        raise InstallError("the platform does not provide a no-replace directory rename")
    rename.argtypes = (ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint)
    rename.restype = ctypes.c_int
    if rename(parent_fd, os.fsencode(staged), parent_fd, os.fsencode(prefix_path.name), flags):
        error = ctypes.get_errno()
        if error == errno.EEXIST:
            raise InstallError(f"{prefix_path}: a bundle requires a new install prefix")
        raise OSError(error, os.strerror(error), prefix_path)


def install_bundle(files, prefix_path, *, run_self_check):
    parent = Prefix(prefix_path.parent, check_owner=False)
    try:
        status = os.fstat(parent.fd)
        if (status.st_uid not in (0, os.geteuid())
                or (status.st_mode & 0o022 and not status.st_mode & stat.S_ISVTX)):
            raise InstallError(f"{parent.path}: bundle staging requires a protected parent directory")
        if Prefix.kind(parent.fd, prefix_path.name) is not None:
            raise InstallError(f"{prefix_path}: a bundle requires a new install prefix")
        staged = f".witchy-install-{os.urandom(16).hex()}"
        os.mkdir(staged, 0o700, dir_fd=parent.fd)
        staged_path = parent.path / staged
        identity = os.stat(staged, dir_fd=parent.fd, follow_symlinks=False)
        committed = False
        try:
            prefix = Prefix(staged_path)
            try:
                for name, data in files:
                    prefix.write(name, data, 0o755 if name in BUNDLE_EXECUTABLES else 0o644)
                link_tools(prefix)
                checked = check_engine(staged_path)
                if run_self_check:
                    self_check(staged_path)
                prefix.create = False
                verify_prefix(files, prefix)
                current = os.stat(staged, dir_fd=parent.fd, follow_symlinks=False)
                if not os.path.samestat(identity, current) or not os.path.samestat(identity, os.fstat(prefix.fd)):
                    raise InstallError("the staged install directory moved during validation")
                publish_new_prefix(parent.fd, staged, prefix_path)
                committed = True
            finally:
                os.close(prefix.fd)
        finally:
            if not committed:
                try:
                    current = os.stat(staged, dir_fd=parent.fd, follow_symlinks=False)
                except FileNotFoundError:
                    pass
                else:
                    if os.path.samestat(identity, current):
                        shutil.rmtree(staged, dir_fd=parent.fd)
        return checked.replace(str(staged_path), str(prefix_path))
    finally:
        os.close(parent.fd)


def mbx_build(mbx, manifest, binary, target, jobs):
    """One binary, release, from the checkout's lock, with nothing fetched."""
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("DYLD_", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC", "MBX_REMOTE_"))
                   and key not in ("CI", "GITHUB_ACTIONS", "GITLAB_CI")}
    environment.update(CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE="true", CARGO_BUILD_JOBS=str(jobs),
                       # MBX is the build's cache; another wrapper (a Cargo configuration's) is not used.
                       RUSTC_WRAPPER="", RUSTC_WORKSPACE_WRAPPER="", CARGO_BUILD_RUSTC_WRAPPER="", CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER="",
                       # No remote cache: no URL, and a mode that neither reads nor (outside trusted
                       # CI, which this environment is not, and with writes untrusted) writes.
                       MBX_REMOTE_URL="", MBX_REMOTE_MODE="write-only", MBX_REMOTE_TRUST_WRITES="false")
    command = [mbx, "build", "--release", "--locked", "--offline", "--manifest-path", str(manifest), "--bin", binary]
    print("install: " + " ".join(command), flush=True)
    if subprocess.call(command, env=environment) != 0:
        raise InstallError(f"building {binary} failed (offline: every dependency must already be in Cargo's or MBX's local cache)")
    built = target / "release" / binary
    if not built.is_file():
        raise InstallError(f"{built}: not there after the build")
    return built


def crypto_library(host, release):
    """The crypto library the engine is linked against by `@rpath` (macOS), and where the
    build left it. None where the engine links it statically (Linux)."""
    if sys.platform != "darwin":
        return None
    listed = subprocess.run(["otool", "-L", str(host)], capture_output=True, text=True, check=True).stdout
    names = re.findall(r"@rpath/(libaws_lc_fips_[A-Za-z0-9_]+_crypto\.dylib)", listed)
    if len(names) != 1:
        raise InstallError(f"{host}: expected one @rpath crypto library in otool -L, found {names}")
    name = names[0]
    # Cargo names a build script's directory `aws-lc-fips-sys-HASH`; MBX `aws-lc-fips-sys/HASH`.
    built = [*release.glob(f"build/aws-lc-fips-sys-*/out/build/artifacts/{name}"),
             *release.glob(f"build/aws-lc-fips-sys/*/out/build/artifacts/{name}")]
    candidates = [path for path in [release / name, *sorted(built)] if path.is_file()]
    if not candidates:
        raise InstallError(f"{name}: not in the build ({release}, or build/aws-lc-fips-sys[-/]*/out/build/artifacts/)")
    # Several builds of it (other features, an older build): only if they are one file.
    if len({path.read_bytes() for path in candidates}) != 1:
        raise InstallError(f"{name}: the build holds different copies ({', '.join(map(str, candidates))}); build into a new --build-dir")
    return candidates[0]


def run_install_worker(command, *, timeout, **options):
    read_end, write_end = os.pipe()
    try:
        return subprocess.run([*command, f"--lifeline=fd:{read_end}"], env={}, pass_fds=(read_end,),
                              capture_output=True, timeout=timeout, **options)
    finally:
        os.close(write_end)
        os.close(read_end)


def check_bundled_tuf_workers(prefix_path, host):
    """Run each pinned worker against a synthetic request with no registry access."""
    folder = prefix_path / TUF_BUNDLE_DIR
    try:
        info = folder.lstat()
    except FileNotFoundError:
        return False
    if not stat.S_ISDIR(info.st_mode):
        raise InstallError("invalid bundled TUF worker directory")
    pins_file = folder / "pins.json"
    fd = os.open(pins_file, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise InstallError("invalid bundled TUF worker pins")
        wire = stream.read(4097)

    def unique_keys(pairs):
        result = dict(pairs)
        if len(result) != len(pairs):
            raise ValueError("duplicate key")
        return result

    try:
        pins = json.loads(wire, object_pairs_hook=unique_keys) if len(wire) <= 4096 else None
    except (ValueError, RecursionError):
        raise InstallError("invalid bundled TUF worker pins") from None
    if (not isinstance(pins, dict) or pins.keys() != set(TUF_MODULES)
            or any(not isinstance(pin, str) or not re.fullmatch(r"sha384:[0-9a-f]{96}", pin)
                   for pin in pins.values())):
        raise InstallError("invalid bundled TUF worker pins")

    with tempfile.TemporaryDirectory(prefix="witchy-install-tuf-") as work:
        root = Path(work).resolve()
        empty, destination = root / "empty", root / "destination"
        empty.touch()
        read_file, write_file = f"--file={empty}", f"--write-file={destination}"
        # Downloaders import Net. Only the unusable loopback port zero is in scope.
        network = ("--net=127.0.0.1:0", "--net-bytes=0")
        file_budget = ("--fs-bytes=32", "--fs-entries=1")
        # HTTPS rejects the empty CA file. The other workers reject the empty request.
        cases = {
            "tuf_fetch_select": ((f"--dir={root}", "--clock", "--fs-bytes=0", "--fs-entries=0"), b"refused\n"),
            "tuf_get": ((read_file, write_file, *network, *file_budget, "--clock", "--rand"), b"refused 5\n"),
            "tuf_get_loopback": ((write_file, *network, *file_budget), b"refused 2\n"),
            "tuf_extract": ((read_file, write_file, *file_budget, "--clock"), b"refused lock\n"),
        }
        for name, (grants, refusal) in cases.items():
            command = [host, folder / f"{name}.wasm", f"--module-pin={pins[name]}", *grants,
                       "--no-links", "--allocator", "--input-stdin", "--input-bytes=4",
                       "--output", "--output-bytes=32", "--memory-pages=768",
                       f"--module-bytes={WORKER_MODULE_LIMIT}", "--fuel=1000000", "--deadline-ms=20000"]
            try:
                ran = run_install_worker(command, input=b"{}\n", timeout=22)
            except subprocess.TimeoutExpired:
                raise InstallError(f"the bundled TUF worker {name} did not finish before the deadline") from None
            if ran.returncode or ran.stdout != refusal or os.path.lexists(destination):
                detail = ran.stderr.decode("utf-8", errors="replace").strip()[:300]
                raise InstallError(f"the installed host could not check the bundled TUF worker {name} "
                                   f"(status {ran.returncode}, output {ran.stdout[:32]!r}): {detail}")
    return True


def check_bundled_manifest_worker(prefix_path, host):
    module = prefix_path / MANIFEST_MODULE
    pin_file = prefix_path / MANIFEST_PIN
    if not os.path.lexists(module) and not os.path.lexists(pin_file):
        return False
    fd = os.open(pin_file, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise InstallError("invalid bundled package manifest worker pin")
        pin = stream.read(105)
    if re.fullmatch(rb"sha384:[0-9a-f]{96}\n", pin) is None:
        raise InstallError("invalid bundled package manifest worker pin")
    wire = b'[package]\nname = "probe"\nversion = "1.0.0"\nsource = "probe.wy"\n'
    expected = sha384(wire).digest() + b"5\nprobe5\n1.0.08\nprobe.wyend\n"
    with tempfile.TemporaryDirectory(prefix="witchy-install-manifest-") as work:
        path = Path(work).resolve() / "witchy.toml"
        path.write_bytes(wire)
        command = [host, module, f"--module-pin={pin[:-1].decode('ascii')}", f"--file={path}",
                   "--no-links", "--allocator", "--output", f"--fs-bytes={len(wire) + 1}",
                   "--fs-entries=1", "--output-bytes=128", f"--module-bytes={WORKER_MODULE_LIMIT}",
                   "--memory-pages=768", "--fuel=1000000", "--deadline-ms=5000"]
        try:
            ran = run_install_worker(command, stdin=subprocess.DEVNULL, timeout=7)
        except subprocess.TimeoutExpired:
            raise InstallError("the bundled package manifest worker did not finish before its deadline") from None
    if ran.returncode != 0 or ran.stdout not in (b"ok-v2\n" + expected, b"ok\n" + expected):
        detail = ran.stderr.decode("utf-8", errors="replace").strip()[:300]
        raise InstallError(f"the installed host could not check the bundled package manifest worker "
                           f"(status {ran.returncode}, output {ran.stdout[:32]!r}): {detail}")
    return True


def check_engine(prefix_path):
    """The installed engine's crypto library resolves beside it, by its own rpath only, and
    the engine starts with no DYLD_ variable. Returns what was checked, to be said."""
    host = prefix_path / "bin" / "witchy-wasmtime-host"
    said = "the engine starts"
    if sys.platform == "darwin":
        listed = subprocess.run(["otool", "-L", str(host)], capture_output=True, text=True, check=True).stdout
        name = re.search(r"@rpath/(libaws_lc_fips_[A-Za-z0-9_]+_crypto\.dylib)", listed).group(1)
        rpaths = re.findall(r"cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (\S+)", subprocess.run(["otool", "-l", str(host)], capture_output=True, text=True, check=True).stdout)
        if rpaths != ["@executable_path"]:
            raise InstallError(f"{host}: its rpaths are {rpaths}, not @executable_path alone")
        if not (host.parent / name).is_file():
            raise InstallError(f"{host.parent / name}: missing; the engine would not start")
        said = f"otool -L: @rpath/{name} -> {host.parent / name}"
    clean = {key: value for key, value in os.environ.items() if not key.startswith("DYLD_")}
    ran = subprocess.run([str(host)], env=clean, capture_output=True, text=True)
    if ran.returncode != 1 or not ran.stderr.startswith("witchy: "):
        raise InstallError(f"{host}: does not start (status {ran.returncode}): {ran.stderr.strip()[:300]}")
    if check_bundled_tuf_workers(prefix_path, host):
        said += " and all bundled TUF workers run"
    if check_bundled_manifest_worker(prefix_path, host):
        said += " and the bundled package manifest worker runs"
    return said


PROOF_CHECK_SOURCE = """fn first(values: &[U64]) -> U64:
    requires values.len() > 0
    ensures true
    proof:
        auto
    return values[0]

fn main(output: Console<Write>):
    let values: [U64; 2] = [42, 0]
    let slice = values.slice()
    output.print_int(first(slice))
"""


def self_check(prefix_path):
    """Check the installed tools in temporary projects, including the selected proof checker."""
    if not (os.environ.get("WITCHY_WASM_READY_BINDING") or os.environ.get("WITCHY_WASM_LEAN_ROOT")):
        raise InstallError("the proof self-check needs WITCHY_WASM_LEAN_ROOT or WITCHY_WASM_READY_BINDING "
                           "(docs/coven-http.md). To defer it at install time, use --no-self-check, "
                           "then supply a trusted proof checker and run --check-only.")
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith("DYLD_") and key not in ("WITCHYC", "WITCHY_WASMTIME_HOST", "COVEN", "PYTHONPATH", "PYTHONHOME")}
    # Exercise the configured checker even when the caller has cached proofs.
    environment.pop("WITCHY_PROOF_CACHE", None)
    environment["PATH"] = f"{prefix_path / 'bin'}{os.pathsep}{environment.get('PATH', '')}"
    with tempfile.TemporaryDirectory(prefix="witchy-install-check-") as work:
        def invoke(*arguments):
            return subprocess.run([str(prefix_path / "bin" / "grimoire"), *arguments], cwd=work,
                                  env=environment, capture_output=True, text=True)

        def run(*arguments):
            done = invoke(*arguments)
            if done.returncode != 0:
                raise InstallError(f"self-check: grimoire {' '.join(arguments)} failed (status {done.returncode}):\n{done.stdout}{done.stderr}")
            return done
        run("new", "selfcheck")
        project = Path(work) / "selfcheck"
        source = project / "main.wy"
        source.write_text(PROOF_CHECK_SOURCE)
        ran = run("run", "selfcheck")
        if ran.stdout != "42\n":
            raise InstallError(f"self-check: grimoire run printed {ran.stdout!r}")
        source.write_text(PROOF_CHECK_SOURCE.replace("ensures true", "ensures false"))
        refused = invoke("build", "selfcheck")
        if (refused.returncode == 0 or "proof failed: function `first`" not in refused.stderr
                or (project / "target" / "main.wasm").exists()):
            raise InstallError(f"self-check: expected a proof refusal for first:\n{refused.stdout}{refused.stderr}")
        run("new", "selfcheck-lib", "--lib")
        run("build", "selfcheck-lib")
        done = subprocess.run([str(prefix_path / "bin" / "coven"), "--help"], cwd=work, env=environment, capture_output=True, text=True)
        if done.returncode != 0:
            raise InstallError(f"self-check: coven --help failed (status {done.returncode}):\n{done.stderr}")
    print("install: self-check: proved slice access ran, false proof refused, library built through Coven, coven --help: ok")


def main(argv=None):
    cli = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    cli.add_argument("--witchy", type=Path, help="the witchyc source directory")
    cli.add_argument("--coven", type=Path, help="the coven source checkout")
    mode = cli.add_mutually_exclusive_group()
    mode.add_argument("--check-only", action="store_true", help="check an installed prefix; build and install nothing")
    mode.add_argument("--make-bundle", type=Path, help="write a pinned client bundle from prebuilt binaries")
    mode.add_argument("--from-bundle", type=Path, help="install a pinned client bundle into a new prefix")
    mode.add_argument("--verify-bundle", type=Path, help="compare installed files and links with a pinned client bundle")
    cli.add_argument("--compiler-binary", type=Path, help="prebuilt witchyc for --make-bundle")
    cli.add_argument("--host-binary", type=Path, help="prebuilt witchy-wasmtime-host for --make-bundle")
    cli.add_argument("--tuf-workers", type=Path, help="directory of four checked TUF workers for --make-bundle")
    cli.add_argument("--manifest-worker", type=Path, help="checked package manifest Wasm worker for --make-bundle")
    cli.add_argument("--bundle-pin", help="independently provisioned SHA-384 pin for --from-bundle or --verify-bundle")
    cli.add_argument("--prefix", default="~/.local/witchy", help="where to install (default ~/.local/witchy; under the home directory)")
    cli.add_argument("--build-dir", type=Path, help="Cargo's target directory (default PREFIX/build)")
    cli.add_argument("--mbx", default=os.environ.get("MBX", "mbx"), help="the MBX binary (default $MBX, or mbx on PATH)")
    cli.add_argument("-j", "--jobs", type=int, default=4)
    cli.add_argument("--no-self-check", action="store_true", help="install only")
    args = cli.parse_args(argv)
    try:
        grimoire_entry = HERE / "grimoire"
        if not args.make_bundle and (args.compiler_binary or args.host_binary or args.tuf_workers or args.manifest_worker):
            raise InstallError("--compiler-binary, --host-binary, --tuf-workers and --manifest-worker require --make-bundle")
        bundle = args.from_bundle or args.verify_bundle
        if bool(bundle) != bool(args.bundle_pin):
            raise InstallError("--from-bundle or --verify-bundle requires --bundle-pin, and vice versa")
        if args.make_bundle:
            if not (args.compiler_binary and args.host_binary and args.coven) or args.witchy or args.build_dir:
                raise InstallError("--make-bundle requires --compiler-binary, --host-binary, and --coven")
            coven_entry = args.coven.resolve() / "witchy2" / "coven"
            pin = write_bundle(args.make_bundle, args.compiler_binary.resolve(), args.host_binary.resolve(),
                               grimoire_entry, coven_entry, args.tuf_workers, args.manifest_worker)
            print(f"install: client bundle {args.make_bundle} {pin}")
            return 0

        prefix_path = checked_prefix(args.prefix, keep_leaf=bool(bundle))
        if args.verify_bundle:
            if args.witchy or args.coven or args.build_dir or args.no_self_check:
                raise InstallError("--verify-bundle takes the bundle pin and install prefix")
            verify_bundle(bundle_files(args.verify_bundle, args.bundle_pin), prefix_path)
            print(f"install: verified the bundle files and tool links in {prefix_path}")
            return 0
        if args.from_bundle:
            if args.witchy or args.coven or args.build_dir:
                raise InstallError("--from-bundle uses the tools in the bundle")
            files = bundle_files(args.from_bundle, args.bundle_pin)
            print(f"install: {install_bundle(files, prefix_path, run_self_check=not args.no_self_check)}")
            print(f"install: installed. Add {prefix_path / 'bin'} to PATH.")
            return 0
        if args.check_only:
            if args.witchy or args.coven or args.no_self_check:
                raise InstallError("--check-only takes only --prefix")
            print(f"install: {check_engine(prefix_path)}")
            self_check(prefix_path)
            return 0
        if args.witchy is None or args.coven is None:
            raise InstallError("--witchy and --coven name the source checkouts to build and install from")
        witchy, coven_dir = args.witchy.resolve(), args.coven.resolve()
        grimoire_entry, coven_entry = HERE / "grimoire", coven_dir / "witchy2" / "coven"
        for path in (witchy / "Cargo.toml", witchy / "tools/wasmtime-host/Cargo.toml", grimoire_entry, coven_entry):
            if not path.is_file():
                raise InstallError(f"{path}: not there; --witchy and --coven name the source checkouts")
        # Every module is found before anything is built or written.
        tool_files(grimoire_entry, GRIMOIRE_DATA)
        tool_files(coven_entry, COVEN_DATA)
        mbx = shutil.which(args.mbx)
        if mbx is None:
            raise InstallError(f"cannot run {args.mbx!r}: give --mbx or set MBX")
        if args.jobs < 1:
            raise InstallError("--jobs is at least 1")
        prefix = Prefix(prefix_path)
        # The build is the prefix's own, made and checked as the rest of it is (no link followed),
        # unless the operator names another directory, which is then theirs to vouch for.
        if args.build_dir is None:
            os.close(prefix.directory("build"))
            target = prefix_path / "build"
        else:
            target = args.build_dir.resolve()
        compiler = mbx_build(mbx, witchy / "Cargo.toml", "witchyc", target, args.jobs)
        host = mbx_build(mbx, witchy / "tools/wasmtime-host/Cargo.toml", "witchy-wasmtime-host", target, args.jobs)
        library = crypto_library(host, target / "release")
        prefix.write("bin/witchyc", compiler.read_bytes(), 0o755)
        if library is not None:
            prefix.write(Path("bin") / library.name, library.read_bytes(), 0o644)
        prefix.write("bin/witchy-wasmtime-host", host.read_bytes(), 0o755)
        install_scripts(prefix, grimoire_entry, coven_entry)
        print(f"install: {check_engine(prefix_path)}")
        print(f"install: installed. Add {prefix_path / 'bin'} to PATH.")
        if not args.no_self_check:
            self_check(prefix_path)
        return 0
    except (InstallError, OSError, subprocess.CalledProcessError) as error:
        print(f"install: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
