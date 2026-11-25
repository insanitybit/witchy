"""Call the ordinary Coven CLI in an isolated, single-threaded unittest worker.

CLI code, grammar, validated source bytes and the compiler digest are shared. Each command
gets new module globals, including verification, batch, and compiled-module state.
Process/descriptor/environment and racing-launcher cases use the executable.
"""
import contextlib
import functools
import importlib.machinery
import importlib.util
import io
import json
import locale
import os
from pathlib import Path
import selectors
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time


def _remember_catalog_searches():
    """gettext's search for a message catalog, kept for this test process: argparse asks for one
    for each help string of each in-process command's parser, and each search stats every
    candidate path. Each answer is kept under all it depends on: the domain, directory and
    languages asked for, and the locale variables."""
    import gettext
    find, found = gettext.find, {}

    def remembered(domain, localedir=None, languages=None, all=False):
        key = (domain, localedir, None if languages is None else tuple(languages), all,
               tuple(os.environ.get(name) for name in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG")))
        if key not in found:
            found[key] = find(domain, localedir, languages, all)
        result = found[key]
        return list(result) if isinstance(result, list) else result
    gettext.find = remembered


_remember_catalog_searches()


_SOURCES = {}
_PARSERS = {}


@functools.cache
def compiler_digest_cache(identity):
    return {}


@functools.cache
def code(path):
    loader = importlib.machinery.SourceFileLoader("registry_command", str(path))
    return loader.get_code(loader.name)


def fresh(path):
    path = Path(path).resolve()
    loader = importlib.machinery.SourceFileLoader("registry_command", str(path))
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    exec(code(path), module.__dict__)
    if hasattr(module, "argument_parser"):
        build_parser = module.argument_parser
        def parser(**options):
            key = path, tuple(sorted(options.items()))
            if key not in _PARSERS:
                _PARSERS[key] = build_parser(**options)
            return _PARSERS[key]
        module.argument_parser = parser
    if _RESIDENT[0]:
        module.subprocess = ResidentSubprocess()
    return module


# A resident engine for the suite (Witchy 2's `witchy-wasmtime-host --resident=fd:N`, built with
# `--features resident`): each engine run is still a process of its own, forked from one process
# that keeps compiled modules, instead of a new executable each time. Only a launch of exactly
# the suite's host (`WITCHY_TEST_RESIDENT_FOR`) with the arguments modelled below is sent there;
# anything else, a test's own wrapper or toolchain included, starts a real process. The cases
# that check startup, inherited descriptors and the environment run the executable regardless.
SOCKET_VARIABLE, FOR_VARIABLE = "WITCHY_TEST_RESIDENT_SOCKET", "WITCHY_TEST_RESIDENT_FOR"
_RESIDENT = [os.environ.get(SOCKET_VARIABLE), os.environ.get(FOR_VARIABLE)]
_UMASK_LOCK = threading.Lock()
_ROUTED = {"env", "stdin", "stdout", "stderr", "pass_fds", "cwd", "text", "capture_output", "check", "timeout", "input"}


@contextlib.contextmanager
def resident_host(binary, host):
    """Start the resident engine `binary` for runs of `host`; yields the variables that let the
    suite's test processes use it. It stops when its standard input closes."""
    executable = shutil.which(host)
    if executable is None:
        raise RuntimeError(f"cannot find ordinary Wasmtime host {host!r}")
    host_path = os.path.realpath(executable)
    with tempfile.TemporaryDirectory(prefix="witchy-resident-") as private:
        path = os.path.join(private, "socket")
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            listener.bind(path)
            listener.listen(256)
            server = subprocess.Popen([binary, f"--resident=fd:{listener.fileno()}"], stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, env={}, pass_fds=(listener.fileno(),))
        finally:
            listener.close()
        try:
            if server.stdout.readline() != b"ready\n":
                raise RuntimeError(f"resident engine {binary} did not start")
            previous = list(_RESIDENT)
            _RESIDENT[:] = [path, host_path]
            try:
                yield {SOCKET_VARIABLE: path, FOR_VARIABLE: host_path}
            finally:
                _RESIDENT[:] = previous
        finally:
            server.stdin.close()
            try:
                server.wait(timeout=30)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
            server.stdout.close()


def _routed(args, options, popen=False):
    if not _RESIDENT[0] or isinstance(args, (str, bytes)) or not args or set(options) - _ROUTED:
        return False
    # Interactive pipes, merged output and text Popen streams still use the executable.
    if options.get("stdin") is subprocess.PIPE or options.get("stderr") is subprocess.STDOUT:
        return False
    if popen and (options.get("text") or set(options) & {"input", "timeout", "capture_output", "check"}):
        return False
    if options.get("input") is not None and (not isinstance(options["input"], bytes) or options.get("stdin") is not None):
        return False
    return os.path.realpath(os.fspath(args[0])) == _RESIDENT[1]


class ResidentSubprocess:
    """Stands in for `subprocess` in one fresh command module (`fresh`)."""

    def __getattr__(self, name):
        return getattr(subprocess, name)

    def Popen(self, args, **options):
        if not _routed(args, options, popen=True):
            return subprocess.Popen(args, **options)
        return ResidentProcess(args, **options)

    def run(self, args, **options):
        if not _routed(args, options):
            return subprocess.run(args, **options)
        check = options.pop("check", False)
        timeout = options.pop("timeout", None)
        data = options.pop("input", None)
        if options.pop("capture_output", False):
            if options.get("stdout") is not None or options.get("stderr") is not None:
                raise ValueError("stdout and stderr arguments may not be used with capture_output.")
            options["stdout"] = options["stderr"] = subprocess.PIPE
        with contextlib.ExitStack() as stack:
            request_pipe = data is not None and "--sqlite-request-stdin" in args
            if request_pipe:
                options["stdin"] = subprocess.PIPE
            elif data is not None:
                # These commands give the guest a complete input. The executable
                # boundary tests cover pipe behavior.
                stream = stack.enter_context(tempfile.TemporaryFile())
                stream.write(data)
                stream.seek(0)
                options["stdin"] = stream
            process = ResidentProcess(args, **options)
            try:
                stdout, stderr = process.communicate(input=data if request_pipe else None, timeout=timeout)
            except BaseException:
                process.close()
                raise
        if check and process.returncode:
            raise subprocess.CalledProcessError(process.returncode, args, stdout, stderr)
        return subprocess.CompletedProcess(args, process.returncode, stdout, stderr)


class ResidentProcess:
    """One run in the resident engine, as much of `Popen` as the registry commands use: the
    process the engine forks gets these standard streams, `pass_fds` at their own numbers, this
    environment, umask and working directory, and nothing else (as `close_fds` gives)."""

    def __init__(self, args, env=None, stdin=None, stdout=None, stderr=None, pass_fds=(), cwd=None, text=False):
        self.args, self.text, self.returncode, self.pid, self._status = args, text, None, None, b""
        self.stdout = self.stderr = self.stdin = self._socket = None
        opened, given = [], []

        def stream(spec, number, mode):
            if spec is None:
                return number
            if spec is subprocess.DEVNULL:
                fd = os.open(os.devnull, mode)
                opened.append(fd)
                return fd
            if spec is subprocess.PIPE:
                reading, writing = os.pipe()
                opened.append(writing)
                return reading, writing
            return spec if isinstance(spec, int) else spec.fileno()

        try:
            if stdin is subprocess.PIPE:
                reading, writing = os.pipe()
                opened.append(reading)
                self.stdin = open(writing, "wb", buffering=0)
                os.set_blocking(writing, False)
                stdin = reading
            fds = {0: stream(stdin, 0, os.O_RDONLY), 1: stream(stdout, 1, os.O_WRONLY)}
            fds[2] = stream(stderr, 2, os.O_WRONLY)
            for number in (1, 2):
                if isinstance(fds[number], tuple):
                    reading, fds[number] = fds[number]
                    setattr(self, "stdout" if number == 1 else "stderr", open(reading, "rb", buffering=0))
            directory = os.open(cwd if cwd is not None else ".", os.O_RDONLY | os.O_DIRECTORY)
            opened.append(directory)
            given = [(fds[0], 0), (fds[1], 1), (fds[2], 2), (directory, 0xFFFFFFFF)] + [(fd, fd) for fd in pass_fds]
            environment = os.environ if env is None else env
            with _UMASK_LOCK:
                mask = os.umask(0o022)
                os.umask(mask)
            strings = lambda items: struct.pack("<I", len(items)) + b"".join(struct.pack("<I", len(item)) + item for item in items)
            body = strings([os.fsencode(os.fspath(arg)) for arg in args]) + \
                strings([os.fsencode(name) + b"=" + os.fsencode(value) for name, value in environment.items()])
            self._socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self._socket.connect(_RESIDENT[0])
            socket.send_fds(self._socket, [b"WRS1" + struct.pack("<I", len(given))], [fd for fd, _ in given])
            self._socket.sendall(struct.pack(f"<{len(given)}I", *(number for _, number in given)) + struct.pack("<II", mask, len(body)) + body)
        except BaseException:
            self.close()
            raise
        finally:
            for fd in opened:
                os.close(fd)

    def close(self):
        # Disconnecting asks the resident engine to retire this run.
        if self._socket is not None:
            self._socket.close()
        for stream in (self.stdin, self.stdout, self.stderr):
            if stream is not None:
                stream.close()

    def _decode(self, data):
        if data is None or not self.text:
            return data
        return data.decode(locale.getencoding()).replace("\r\n", "\n").replace("\r", "\n")

    def communicate(self, input=None, timeout=None):
        if input is not None and self.stdin is None:
            raise ValueError("input requires a resident stdin pipe")
        pending = memoryview(input) if input is not None else memoryview(b"")
        chunks = {}
        deadline = None if timeout is None else time.monotonic() + timeout
        with selectors.DefaultSelector() as selector:
            if self.stdin is not None:
                if pending:
                    selector.register(self.stdin, selectors.EVENT_WRITE)
                else:
                    self.stdin.close()
            for stream in (self.stdout, self.stderr):
                if stream is not None:
                    selector.register(stream, selectors.EVENT_READ)
                    chunks[stream] = []
            while selector.get_map():
                left = None if deadline is None else deadline - time.monotonic()
                if left is not None and left <= 0:
                    raise subprocess.TimeoutExpired(self.args, timeout)
                for key, _ in selector.select(left):
                    if key.fileobj is self.stdin:
                        try:
                            pending = pending[os.write(self.stdin.fileno(), pending[:65536]):]
                        except BlockingIOError:
                            continue
                        except BrokenPipeError:
                            pending = pending[len(pending):]
                        if not pending:
                            selector.unregister(self.stdin)
                            self.stdin.close()
                        continue
                    data = os.read(key.fileobj.fileno(), 65536)
                    if data:
                        chunks[key.fileobj].append(data)
                    else:
                        selector.unregister(key.fileobj)
                        key.fileobj.close()
        self.wait(None if deadline is None else max(0, deadline - time.monotonic()))
        joined = lambda stream: self._decode(b"".join(chunks[stream])) if stream is not None else None
        return joined(self.stdout), joined(self.stderr)

    def wait(self, timeout=None):
        if self.returncode is None:
            self._socket.settimeout(timeout)
            try:
                while len(self._status) < 8:
                    data = self._socket.recv(8 - len(self._status))
                    if not data:
                        raise RuntimeError("the resident engine lost the run")
                    self._status += data
            except (TimeoutError, BlockingIOError):
                raise subprocess.TimeoutExpired(self.args, timeout) from None
            status = self._status
            if status[:4] != b"WRS1":
                raise RuntimeError("the resident engine answered something else")
            self.returncode = struct.unpack("<i", status[4:])[0]
            self._socket.close()
        return self.returncode

    def poll(self):
        if self.returncode is None:
            try:
                return self.wait(timeout=0)
            except subprocess.TimeoutExpired:
                return None
        return self.returncode

    def terminate(self):
        if self.returncode is None:
            self._socket.send(b"T")

    def kill(self):
        if self.returncode is None:
            self._socket.send(b"K")


@contextlib.contextmanager
def command_environment(env):
    if env is None:
        yield
        return
    original = os.environ.copy()
    try:
        os.environ.clear()
        os.environ.update(env)
        yield
    finally:
        os.environ.clear()
        os.environ.update(original)


from test_signatures import configure as configure_signatures, policy_signatures


def fresh_coven(path):
    module = fresh(path)
    # Inline calls never take authority over any descriptor of the test runner.
    module.INHERITED_DESCRIPTORS = frozenset()
    compiler = shutil.which(os.environ.get("WITCHYC", "witchyc"))
    if compiler:
        st = os.stat(compiler)
        module._COMPILER_DIGEST = compiler_digest_cache((compiler, st.st_dev, st.st_ino, st.st_size, st.st_ctime_ns))
    # Coven's sources as last read: each entry is checked against its file's identity on use.
    if hasattr(module, "_SOURCES"):
        module._SOURCES = _SOURCES
    if hasattr(module, "witchy_run_signer"):
        configure_signatures(module)
    return module


def provision_staging(path):
    from coven_staging_schema import schema_worker

    cli = fresh_coven(Path(__file__).with_name("coven"))
    host = cli.tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")
    with schema_worker(cli.build_module, host) as initialize:
        initialize(path)


def empty_staging(path):
    template = os.environ.get("COVEN_TEST_STAGING_TEMPLATE")
    if template:
        shutil.copytree(template, path)
    else:
        provision_staging(path)


def use_resident_http(module):
    if _RESIDENT[0]:
        module.http().subprocess = ResidentSubprocess()


def protocol_http_server(command):
    if _RESIDENT[0]:
        return [sys.executable, __file__, "--protocol-http-server", *command]
    return [sys.executable, *command]


def invoke_coven(command, cwd, env=None, *, resident_http=False):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.chdir(cwd), command_environment(env), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        sys.audit("unittest.cli_inline", "coven", command[1])
        module = fresh_coven(command[0])
        if resident_http:
            use_resident_http(module)
        sys.path.insert(0, str(Path(command[0]).resolve().parent))
        try:
            status = module.main(list(map(str, command[1:])))
        finally:
            sys.path.pop(0)
            temp = module._MODULES.get("work")
            if temp is not None:
                module.atexit.unregister(temp.cleanup)
                temp.cleanup()
    return subprocess.CompletedProcess(command, status, out.getvalue(), err.getvalue())


# A suite's cases, each in a process of its own, forked from one that has imported the test
# modules (`run_cases`). The forking process is started for the purpose with only the standard
# streams, so a case's process holds nothing of the runner's; each case runs as
# `python3 FILE TEST` would, with its own exit handlers, and its output kept apart.

def run_cases(cases, jobs, env, preload=(), resident_launchers=()):
    """Run each (file, test) in `cases`, at most `jobs` at a time, in that order; yields (case,
    status, output, seconds) in the order they finish. `preload` names the CLIs (Coven's and
    Grimoire's) the cases call in process, made ready once before the forks (`_zygote`).
    `resident_launchers` names modules whose fixed engine commands can also use the resident host."""
    config = {"cases": [[str(path), test] for path, test in cases], "jobs": jobs,
              "preload": [str(p) for p in preload], "resident_launchers": list(resident_launchers)}
    with tempfile.TemporaryFile() as spec:
        spec.write(repr(config).encode())
        spec.seek(0)
        zygote = subprocess.Popen([sys.executable, __file__, "--zygote"], stdin=spec, stdout=subprocess.PIPE, env=env)
    try:
        for line in zygote.stdout:
            index, status, length, seconds = line.split()
            output = zygote.stdout.read(int(length)).decode("utf-8", "replace")
            yield tuple(cases[int(index)]), int(status), output, float(seconds)
    finally:
        zygote.stdout.close()
        if zygote.wait():
            raise RuntimeError(f"the forking process stopped ({zygote.returncode})")


def _zygote():
    import ast
    import atexit
    import traceback
    import unittest
    import weakref
    config = ast.literal_eval(sys.stdin.buffer.read().decode())
    initial_release = os.environ.get("COVEN_TEST_INITIAL_RELEASE")
    staging_template = os.environ.get("COVEN_TEST_STAGING_TEMPLATE")
    # A case reads standard input as it would under the runner: from nothing.
    null = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null, 0)
    os.close(null)
    results = os.fdopen(os.dup(1), "wb")
    os.set_inheritable(results.fileno(), False)
    modules = {}
    for path, _ in config["cases"]:
        if path not in modules:
            directory, name = os.path.split(path)
            if directory not in sys.path:
                sys.path.insert(0, directory)
            modules[path] = importlib.import_module(name[:-3])
    # What every case would otherwise make again on its first command: the CLIs' code and the
    # modules they import (a module made here is discarded), Coven's compiler digest and sources
    # (kept by their files' identities, `fresh_coven`), the temporary directory (found by
    # writing a file there), and the date parser's tables for this locale and time zone (built
    # by `strptime` on first use, about 40 ms). Each is the same in every case.
    if _RESIDENT[0]:
        launcher = ResidentSubprocess()
        for name in config["resident_launchers"]:
            importlib.import_module(name).subprocess = launcher
    clients = set()
    for path in config["preload"]:
        module = fresh_coven(path)
        if hasattr(module, "_compiler_digest") and shutil.which(os.environ.get("WITCHYC", "witchyc")):
            compiler = module.tool("WITCHYC", "witchyc")
            client = module.compiler_client()
            if sys.platform == "darwin":
                module._compiler_digest(compiler, client.executable_identity(os.stat(compiler)))
            # Let cooperating forked cases inherit this private compiler. Each
            # request still owns its files, grants, policy and diagnostics.
            clients.add(client)
            client.selected(compiler, module.toolchain_environment())
    import datetime
    datetime.datetime.strptime("2000-01-01T00:00:00Z", "%Y-%m-%dT%H:%M:%SZ")
    tempfile.gettempdir()
    # Build fixtures with the same loaded CLI and private engine the forks use.
    if initial_release:
        from test_registry import make_initial_release
        make_initial_release(Path(initial_release), release_only=staging_template is None)
    if staging_template:
        provision_staging(Path(staging_template))
    pending, running = list(enumerate(config["cases"])), {}
    while pending or running:
        while pending and len(running) < config["jobs"]:
            index, (path, test) = pending.pop(0)
            output = tempfile.TemporaryFile()
            sys.stdout.flush()
            sys.stderr.flush()
            pid = os.fork()
            if pid == 0:
                status = 2
                try:
                    results.close()
                    for _, held, _ in running.values():
                        held.close()
                    os.dup2(output.fileno(), 1)
                    os.dup2(output.fileno(), 2)
                    output.close()
                    # Only this case's exit handlers run when it ends, as in a process of its own.
                    atexit._clear()
                    for finalizer in list(weakref.finalize._registry):
                        finalizer.atexit = False
                    sys.argv = [path, test]
                    program = unittest.main(module=modules[path], argv=[path, test], exit=False)
                    status = 0 if program.result.wasSuccessful() else 1
                    for client in clients:
                        client.close()
                    atexit._run_exitfuncs()
                    weakref.finalize._exitfunc()
                except BaseException:
                    status = 2
                    traceback.print_exc()
                finally:
                    try:
                        sys.stdout.flush()
                        sys.stderr.flush()
                    finally:
                        os._exit(status)
            running[pid] = (index, output, time.monotonic())
        pid, wait = os.wait()
        if pid not in running:
            continue
        index, output, started = running.pop(pid)
        output.seek(0)
        text = output.read()
        output.close()
        results.write(f"{index} {os.waitstatus_to_exitcode(wait)} {len(text)} {time.monotonic() - started:.3f}\n".encode() + text)
        results.flush()
    for client in clients:
        client.close()


def longest_first(cases, history):
    """`cases` ordered by how long each took in the last run (`history`, a JSON file of seconds by
    test), longest first, a test not seen before first of all; the order is otherwise kept."""
    try:
        with open(history) as stream:
            seconds = json.load(stream)
    except (OSError, ValueError):
        seconds = {}
    if not isinstance(seconds, dict):
        seconds = {}
    took = lambda case: seconds.get(case[1]) if isinstance(seconds.get(case[1]), (int, float)) else float("inf")
    return sorted(cases, key=lambda case: -took(case))


def record_durations(runs, history):
    """Keep each test's seconds from `runs` (as `run_cases` yields them) for `longest_first`."""
    try:
        staged = f"{history}.{os.getpid()}"
        with open(staged, "w") as stream:
            json.dump({case[1]: seconds for case, _, _, seconds in runs}, stream)
        os.replace(staged, history)
    except OSError:
        pass


if __name__ == "__main__" and sys.argv[1:] == ["--zygote"]:
    # Test modules import the same support state that their forking process uses.
    sys.modules["cli_test_support"] = sys.modules[__name__]
    _zygote()

if __name__ == "__main__" and sys.argv[1:2] == ["--protocol-http-server"]:
    # The protocol tests opt in to this launcher. Keep lifecycle tests on the executable.
    module = fresh(sys.argv[2])
    use_resident_http(module)
    try:
        result = module.main(sys.argv[3:])
    finally:
        if client := sys.modules.get(module.COMPILER_CLIENT_MODULE):
            client.close()
    sys.exit(result)
