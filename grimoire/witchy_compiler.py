"""Client for Witchy's operation-scoped compiler protocol (one connection per request)."""
import atexit
import contextlib
import functools
import hashlib
import os
from pathlib import Path
import re
import select
import shutil
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time

_lock = threading.RLock()
_sessions = {}
_UNSELECTED = object()


def executable_identity(status):
    return status.st_dev, status.st_ino, status.st_size, status.st_mtime_ns, status.st_ctime_ns

_MAGIC = (b'\x7fELF', b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xca\xfe\xba\xbe', b'\xca\xfe\xba\xbf')

@functools.lru_cache(32)
def _native(path, identity):
    # Scripts are command wrappers. They may implement, inspect or change the
    # individual commands rather than the session protocol.
    with open(path, 'rb') as source:
        return source.read(4) in _MAGIC

_WORD = struct.Struct("!I")
_MAX_TEXT = 32 * 1024 * 1024
_STARTUP_SECONDS = 10


def _startup_header(stream, deadline):
    header = bytearray()
    while len(header) < 128:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([stream], [], [], remaining)[0]:
            raise OSError("compiler session startup timed out")
        chunk = os.read(stream.fileno(), 128 - len(header))
        if not chunk:
            break
        newline = chunk.find(b"\n")
        if newline != -1:
            header.extend(chunk[:newline + 1])
            break
        header.extend(chunk)
    return bytes(header)


def _answer(stream):
    def read(size):
        data = stream.read(size)
        if len(data) != size:
            raise OSError("compiler session closed before completing the request")
        return data

    def number():
        return _WORD.unpack(read(_WORD.size))[0]

    def text_field():
        size = number()
        if size > _MAX_TEXT:
            raise OSError("compiler reply exceeds 32 MiB")
        return read(size).decode("utf-8")

    return number(), text_field(), text_field(), text_field(), text_field()


class Compiler:
    def __init__(self, path, env, identity):
        self.path = path
        self.identity = identity
        self.directory = tempfile.mkdtemp(prefix='witchy-c-', dir='/tmp')
        self.address = os.path.join(self.directory, 'socket')
        self.owner = os.getpid()
        self.process = None
        self.started = False
        try:
            self.process = subprocess.Popen([path, 'compiler-session', self.address],
                                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                            stderr=subprocess.PIPE, env=env, close_fds=True)
            deadline = time.monotonic() + _STARTUP_SECONDS
            header = _startup_header(self.process.stdout, deadline)
            if header != b'WITCHY_COMPILER_SESSION_1\n':
                try:
                    diagnostic = self.process.communicate(timeout=max(0, deadline - time.monotonic()))[1]
                except subprocess.TimeoutExpired as error:
                    raise OSError('compiler session startup timed out') from error
                if self.process.returncode == 1 and b'usage: witchyc' in diagnostic:
                    raise Unsupported()
                raise OSError('compiler session startup refused: ' + diagnostic.decode(errors='replace')[:2048])
            self.check_executable()
            self.started = True
            sys.audit('witchy.compiler.start', path, self.process.pid)
            atexit.register(self.close)
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.owner != os.getpid():
            return
        self.owner = None
        atexit.unregister(self.close)
        try:
            if self.process is not None:
                self.process.stdin.close()
                try:
                    if not self.started:
                        self.process.kill()
                    status = self.process.wait(timeout=10)
                    if status and self.started:
                        raise OSError(f"compiler shutdown failed ({status})")
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait()
                    raise
                finally:
                    self.process.stdout.close()
                    self.process.stderr.close()
        finally:
            shutil.rmtree(self.directory)

    def check_executable(self):
        if executable_identity(os.stat(self.path)) != self.identity:
            raise OSError("selected compiler executable changed")

    def request(self, args):
        self.check_executable()
        started = time.monotonic_ns()
        fields = []
        for index, arg in enumerate(args):
            data = os.fsencode(arg)
            is_document = (args[0], index) in (("prepare-grants", 3), ("wasm-grants", 4))
            if len(data) > (_MAX_TEXT if is_document else 65536):
                raise ValueError("compiler request exceeds field limit")
            fields.extend((_WORD.pack(len(data)), data))
        with socket.socket(socket.AF_UNIX) as connection:
            connection.connect(self.address)
            connection.sendall(_WORD.pack(len(args)) + b"".join(fields))
            with connection.makefile("rb") as stream:
                code, out, err, ticket, counters = _answer(stream)
        self.check_executable()
        traced = args[:3] if args[0] in ("prepare-grants", "wasm-grants") else args
        sys.audit("witchy.compiler.request", self.process.pid, tuple(map(os.fspath, traced)),
                  code, counters, time.monotonic_ns() - started)
        return code, out, err, ticket

def close():
    """Finish all compilers owned by this process. Borrowers only discard their references."""
    with _lock:
        previous = list(_sessions.values())
        _sessions.clear()
    first_error = None
    for compiler in previous:
        if compiler is not None:
            try:
                compiler.close()
            except (OSError, subprocess.SubprocessError) as error:
                first_error = first_error or error
    if first_error is not None:
        raise first_error

atexit.register(close)

class Unsupported(Exception):
    pass

def _after_fork():
    global _lock
    _lock = threading.RLock()
    for c in _sessions.values():
        if c is not None and c.owner is not None:
            # Forked callers can use the private socket but do not own the control pipe.
            c.owner = None
            for f in (c.process.stdin, c.process.stdout, c.process.stderr):
                f.close()

os.register_at_fork(after_in_child=_after_fork)

def selected(path, env):
    # These configure the product registry, never the Rust compiler. Keep the
    # actual compiler environment identical for inline and independent callers.
    env = {k: v for k, v in env.items() if k not in ('COVEN', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'WITCHY_WASMTIME_HOST')}
    path = os.path.abspath(shutil.which(os.fspath(path), path=env.get("PATH")) or path)
    st = os.stat(path)
    identity = executable_identity(st)
    try:
        if not _native(path, identity):
            return None
    except PermissionError:
        return None
    # Relative tool selections belong to their original working directory.
    relative_paths = any(
        env.get(k) and not os.path.isabs(env[k]) for k in
        ('HOME','TMPDIR','WITCHY_PROOF_CACHE','WITCHY_WASM_READY_BINDING','WITCHY_LEAN_BIN','WITCHY_WASM_LEAN_ROOT','WITCHY_WASM_NATIVE_HELPER'))
    relative_paths |= any(not os.path.isabs(p) for p in env.get('PATH', os.defpath).split(os.pathsep))
    key = path, identity, tuple(sorted(env.items())), os.getcwd() if relative_paths else None
    with _lock:
        if key not in _sessions:
            try:
                _sessions[key] = Compiler(path, env, identity)
            except Unsupported:
                _sessions[key] = None
        return _sessions[key]

def _publish(entry, output, compile_to, *, module_limit=None):
    entry, output = Path(entry), Path(output)
    def distinct():
        if output.exists() and entry.exists() and output.samefile(entry):
            raise OSError('source and module output must be different files')
    distinct()
    with tempfile.TemporaryDirectory(prefix='.witchy-build-', dir=output.parent) as directory:
        stage = Path(directory) / 'module.wasm'
        answer = compile_to(stage)
        if answer[0] == 0:
            if module_limit is not None:
                info = stage.lstat()
                if not stat.S_ISREG(info.st_mode):
                    raise OSError("the built module is not a regular file")
                if info.st_size > module_limit:
                    raise OSError(f"the built module exceeds the module size limit ([run] module-bytes = {module_limit})")
            distinct()
            os.replace(stage, output)
        return answer

def _result(cmd, answer, capture_output, check, rewrite=None):
    code, out, err, _ = answer
    if rewrite is not None:
        # What the compiler says, in the caller's names for the files it was given.
        out, err = rewrite(out), rewrite(err)
    if not capture_output:
        sys.stdout.write(out)
        sys.stderr.write(err)
    result = subprocess.CompletedProcess(cmd, code, out if capture_output else None, err if capture_output else None)
    if check:
        result.check_returncode()
    return result

def _rewritten(cmd, env, rewrite, capture_output, check, cwd=None):
    """A compiler that is not a session (a wrapper script), with its output in the caller's names."""
    result = subprocess.run(cmd, env=env, cwd=cwd, capture_output=True, text=True, encoding='utf-8')
    return _result(cmd, (result.returncode, result.stdout, result.stderr, ''), capture_output, check, rewrite)

def run(cmd, *, env, cwd=None, capture_output=False, check=False, text=True, encoding='utf-8', input_key=None, document_text=None, selected_compiler=_UNSELECTED, rewrite=None):
    compiler = selected(cmd[0], env)
    if selected_compiler is not _UNSELECTED and compiler is not selected_compiler:
        raise OSError("selected compiler changed since cache lookup")
    if compiler is None:
        if rewrite is not None:
            return _rewritten(cmd, env, rewrite, capture_output, check, cwd)
        return subprocess.run(cmd, env=env, cwd=cwd, capture_output=capture_output, check=check, text=text, encoding=encoding)
    base = os.path.abspath(cwd or os.curdir)
    def path(x):
        return os.path.join(base, x)
    op, *args = cmd[1:]
    if op == 'build':
        entry, flag, output, *options = args
        if flag != '-o':
            raise ValueError('build requires -o')
        sizes = {'--heap-pages': '3', '--slots': '64', '--stack-bytes': '16384'}
        for option in options:
            key, _, value = option.partition('=')
            if key not in sizes:
                raise ValueError('unrecognized build option')
            sizes[key] = value
        wire = [op, path(entry), path(output), *sizes.values()]
        if input_key is not None:
            wire.append(input_key)
    elif op == 'wasm-grants':
        wire = [op, *map(path, args)]
        if input_key is not None:
            wire.extend([input_key, document_text])
    elif op == 'wasm-check-package':
        wire = [op, path(args[0])]
    elif op == 'wasm-caps':
        wire = [op, *(arg if str(arg).startswith('--') else path(arg) for arg in args)]
        if args[0] == '--by-file':
            wire.append(base)
    else:
        raise ValueError('unrecognized compiler operation')
    if op == "build":
        answer = _publish(path(entry), path(output), lambda stage: compiler.request([*wire[:2], stage, *wire[3:]]))
    else:
        answer = compiler.request(wire)
    return _result(cmd, answer, capture_output, check, rewrite)

@contextlib.contextmanager
def reviewed_build(path, entry, document, env, document_text=None, rewrite=None, sizes=None, *, document_base=None, module_limit=None):
    """`sizes`: (heap pages, slots, stack bytes) of the module's memory, or None for the
    compiler's defaults. They go with the review, so the reviewed build has exactly those."""
    entry, document = os.path.abspath(entry), os.path.abspath(document)
    cmd = [path, 'wasm-grants', entry, document]
    if document_text is None:
        document_text = Path(document).read_bytes().decode('utf-8')
    compiler = selected(path, env)
    base_option = [] if document_base is None else [f'--document-base={document_base}']
    sized = [] if sizes is None else [f'--heap-pages={sizes[0]}', f'--slots={sizes[1]}', f'--stack-bytes={sizes[2]}']
    if compiler is None:
        document_pin = hashlib.sha384(document_text.encode('utf-8')).hexdigest()
        result = subprocess.run([*cmd, f'--document-pin=sha384:{document_pin}', *base_option, '--print-input-key'],
                                capture_output=True, text=True, encoding='utf-8', env=env)
        key = None
        if result.returncode == 0:
            header, separator, flags = result.stdout.partition('\n')
            parsed = re.fullmatch(r'input-key: ([0-9a-f]{64})', header)
            if not separator or parsed is None:
                raise OSError('compiler grant review returned no valid input key')
            key = parsed[1]
            result.stdout = flags
        if rewrite is not None:
            result.stderr = rewrite(result.stderr)

        def build(output):
            result.check_returncode()
            output = os.path.abspath(output)
            prefix = [path, 'build', entry, '-o']
            options = [*sized, f'--reviewed-input-key={key}']

            def compile_to(stage):
                ran = subprocess.run([*prefix, stage, *options], env=env, capture_output=rewrite is not None,
                                     text=True, encoding='utf-8')
                return ran.returncode, ran.stdout or '', ran.stderr or '', ''

            answer = _publish(entry, output, compile_to, module_limit=module_limit)
            return _result([*prefix, output, *options], answer, False, True, rewrite)

        yield result, build
        return
    answer = compiler.request(['prepare-grants', entry, document, document_text, *base_option, *([] if sizes is None else map(str, sizes))])
    ticket = answer[3]
    def build(output):
        nonlocal ticket
        if not ticket:
            raise OSError('no live grant review')
        token, ticket = ticket, ''
        # Resolved once, against the caller's directory when it asks for the build.
        output = os.path.abspath(output)
        cmd = [path, 'build', entry, '-o', output]
        answer = _publish(entry, output, lambda stage: compiler.request(['build-checked', token, entry, stage]),
                          module_limit=module_limit)
        return _result(cmd, answer, False, True, rewrite)
    try:
        result = _result(cmd, answer, True, False)
        if rewrite is not None:
            # Standard output is the proposed flags, which name real paths: only what is said.
            result.stderr = rewrite(result.stderr)
        yield result, build
    finally:
        if ticket:
            compiler.request(['release', ticket])
