"""Check shutdown of a Wasmtime action whose Coven launcher is killed."""
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from coven_worker import run_with_lifeline


def check(directory):
    work = Path(directory)
    source = work / "wait.wy"
    module = work / "wait.wasm"
    source.write_text('import console\n'
                      'fn main(output: Console<Write>, input: Console<Read>):\n'
                      '    output.line("ready")\n'
                      '    output.print_int(input.next_byte())\n')
    subprocess.run([os.environ["WITCHYC"], "build", source, "-o", module], check=True, timeout=30)
    command = [sys.executable, __file__, "--action", os.environ["WITCHY_WASMTIME_HOST"], module,
               "--input-stdin", "--output", "--stream-output", "--deadline-ms=8000"]
    with tempfile.TemporaryFile() as errors, subprocess.Popen(
        command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=errors, start_new_session=True,
    ) as launcher:
        try:
            assert select.select((launcher.stdout,), (), (), 4)[0], "action failed to start"
            assert launcher.stdout.readline() == b"ready\n"
            launcher.kill()
            assert launcher.wait(timeout=2) == -signal.SIGKILL
            # Keep stdin open. The worker must exit while still waiting for it.
            assert select.select((launcher.stdout,), (), (), 2)[0], "action outlived its launcher"
            assert launcher.stdout.read(1) == b""
            errors.seek(0)
            assert b"the launcher is gone: the run ends with it" in errors.read()
        except BaseException:
            try:
                os.killpg(launcher.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            raise


if __name__ == "__main__":
    if sys.argv[1:2] == ["--action"]:
        raise SystemExit(run_with_lifeline(subprocess.run, sys.argv[2:], timeout=10).returncode)
    with tempfile.TemporaryDirectory(prefix="coven-action-lifeline-") as directory:
        check(directory)
    print("Wasmtime stopped a blocked action on launcher loss")
