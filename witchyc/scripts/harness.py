#!/usr/bin/env python3
"""What the measurement scripts share: file digests, the tracked tree's snapshot, and
commands run in their own process groups, cancelled and reaped together. (Taken from the
retired native-era test runner, scripts/test-isolated.py.)"""
import hashlib
import os
import signal
import stat
import subprocess
import time
import json
import threading
from pathlib import Path

CANCEL = threading.Event()
PROCESS_LOCK = threading.Lock()
PROCESSES = set()


def process_group_exists(group):
    try:
        os.killpg(group, 0)
        return True
    except ProcessLookupError:
        return False


def terminate(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(4 << 20), b''):
            value.update(block)
    return value.hexdigest()


def tracked(root):
    output = subprocess.check_output(['git', 'ls-files', '-z'], cwd=root)
    return sorted(name for name in output.decode().split('\0') if name)


def snapshot(root, names):
    result = {}
    for name in names:
        path = root / name
        info = path.lstat()
        result[name] = [stat.S_IMODE(info.st_mode),
                        os.readlink(path) if path.is_symlink() else digest(path)]
    return result


def cancel_commands():
    CANCEL.set()
    with PROCESS_LOCK:
        processes = list(PROCESSES)
    for process in processes:
        terminate(process)
    deadline = time.monotonic() + 3
    for process in processes:
        try:
            process.wait(timeout=max(0.01, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


def reap_command_group(process):
    """Only a Popen started with start_new_session=True may enter here."""
    leaked = process.poll() is not None and process_group_exists(process.pid)
    for signum, duration in [(signal.SIGTERM, .5), (signal.SIGKILL, 1.5)]:
        if not process_group_exists(process.pid):
            process.wait(timeout=1)
            return dict(group=process.pid, unexpected_children=leaked, group_absent=True)
        try:
            os.killpg(process.pid, signum)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            process.poll()  # Reap the group leader while waiting for its children.
            if not process_group_exists(process.pid):
                process.wait(timeout=1)
                return dict(group=process.pid, unexpected_children=leaked, group_absent=True)
            time.sleep(.01)
    raise ValueError(f'owned command process group {process.pid} remains after TERM/KILL')


def command(argv, cwd, env, log, timeout=1800, label=None, separate_stderr=False):
    started = time.perf_counter()
    if CANCEL.is_set():
        raise InterruptedError('suite interrupted')
    process = subprocess.Popen(list(map(str, argv)), cwd=cwd, env=env,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE if separate_stderr else subprocess.STDOUT, text=True, start_new_session=True)
    def save_output(output, captured_stderr):
        if separate_stderr:
            Path(str(log) + '.stdout').write_text(output)
            Path(str(log) + '.stderr').write_text(captured_stderr or '')
            log.write_text(output + ('\n[separately captured test stderr]\n' + captured_stderr if captured_stderr else ''))
        else:
            log.write_text(output)
    process.suite_cwd, process.suite_target = str(cwd), label
    with PROCESS_LOCK:
        PROCESSES.add(process)
    output, captured_stderr = '', None
    original_error = None
    cleanup = None
    try:
        try:
            if CANCEL.is_set():
                # Cancellation raced this launch and its snapshot may predate the
                # group; the ordinary reap/drain below stops and records it.
                raise InterruptedError('suite interrupted')
            output, captured_stderr = process.communicate(timeout=timeout)
        except BaseException as error:
            original_error = error
        try:
            cleanup = reap_command_group(process)
        except BaseException as error:
            cleanup = dict(group=process.pid, group_absent=False, error=str(error))
            raise
        finally:
            if original_error is not None:
                # Draining is bounded even if a detached child retained a pipe.
                try:
                    output, captured_stderr = process.communicate(timeout=1)
                except subprocess.TimeoutExpired as error:
                    output = error.output or ''
                    captured_stderr = error.stderr
                    if isinstance(output, bytes):
                        output = output.decode(errors='replace')
                    if isinstance(captured_stderr, bytes):
                        captured_stderr = captured_stderr.decode(errors='replace')
            save_output(output, captured_stderr)
            if cleanup is not None and (not cleanup['group_absent'] or cleanup.get('unexpected_children')):
                Path(str(log) + '.cleanup.json').write_text(json.dumps(dict(cleanup, original_exit_code=process.returncode)) + '\n')
        if original_error is not None:
            if isinstance(original_error, subprocess.TimeoutExpired):
                raise TimeoutError(f'command exceeded {timeout}s; private process group terminated') from original_error
            raise original_error
        if cleanup['unexpected_children']:
            raise ValueError(f'command exited {process.returncode} but left children in owned group {process.pid}; cleaned up, refusing reuse')
    finally:
        # An incompletely reaped group remains owned and visible to cancellation.
        if cleanup is not None and cleanup['group_absent']:
            with PROCESS_LOCK:
                PROCESSES.discard(process)
    result = subprocess.CompletedProcess(argv, process.returncode, output, captured_stderr)
    return result, time.perf_counter() - started
