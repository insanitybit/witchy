"""The compiler client's reviewed build, against a stand-in compiler session (ported from Coven
74071e0). The client is the one the launcher loads, so its pinned digest is checked too."""
from concurrent.futures import ThreadPoolExecutor
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest import mock

from registry_run import load_cli


class CompilerStartup(unittest.TestCase):
    def test_partial_startup_header_times_out_and_reaps_the_child(self):
        client = load_cli().compiler_client()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            executable = root / "compiler"
            executable.write_text("#!/bin/sh\nprintf WITCHY_COMPILER_\nexec /bin/sleep 30\n")
            executable.chmod(0o700)
            session_dir = root / "session"
            session_dir.mkdir()
            processes = []
            popen = client.subprocess.Popen

            def start(*args, **kwargs):
                process = popen(*args, **kwargs)
                processes.append(process)
                return process

            with (mock.patch.object(client, "_STARTUP_SECONDS", 0.25),
                  mock.patch.object(client.tempfile, "mkdtemp", return_value=str(session_dir)),
                  mock.patch.object(client.subprocess, "Popen", side_effect=start)):
                with self.assertRaisesRegex(OSError, "compiler session startup timed out"):
                    client.Compiler(executable, {}, client.executable_identity(executable.stat()))
            self.assertEqual(len(processes), 1)
            self.assertIsNotNone(processes[0].returncode)
            self.assertFalse(session_dir.exists())


class CompilerCancellation(unittest.TestCase):
    def test_shutdown_cancels_an_inflight_standalone_proof(self):
        self.check_shutdown(prepared=False)

    def test_shutdown_cancels_a_prepared_helper_request(self):
        self.check_shutdown(prepared=True)

    def check_shutdown(self, *, prepared):
        compiler = shutil.which(os.environ.get("WITCHYC", "witchyc"))
        if compiler is None:
            self.skipTest("Witchy compiler is required for the cancellation probe")
        client = load_cli().compiler_client()
        compiler_source = Path(__file__).resolve().parents[2] / "witchyc"
        fixture = compiler_source / "tests/fixtures/nopanic/slice-read.wy"
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            lean = root / "sdk/bin/lean"
            lean.parent.mkdir(parents=True)
            marker = root / "started"
            lean.write_text(f"#!/bin/sh\nprintf '%s\\n' \"$$\" > {shlex.quote(str(marker))}\nexec /bin/sleep 30\n")
            lean.chmod(0o700)
            env = dict(os.environ, WITCHY_WASM_LEAN_ROOT=str(lean.parent.parent))
            env.pop("WITCHY_WASM_READY_BINDING", None)
            env.pop("WITCHY_PROOF_CACHE", None)
            scratch = root / "proof-scratch"
            scratch.mkdir()
            env["TMPDIR"] = str(scratch)
            if prepared:
                identity = json.loads(subprocess.check_output(
                    [compiler, "wasm-toolchain-id", str(lean.parent.parent)], env=env, timeout=5))
                source = b"\0".join((compiler_source / name).read_bytes() for name in (
                    "tools/proof-session-body/Resident.lean", "tools/proof-session/native.rs",
                    "src/compiler/proof_audit.lean.in",
                ))
                binding = root / "ready-binding.txt"
                binding.write_text(
                    "witchy.ready-toolchain.v1\n"
                    f"helper={lean}\nhelper_sha256={hashlib.sha256(lean.read_bytes()).hexdigest()}\n"
                    f"sdk={lean.parent.parent}\nsdk_sha256={identity['hex']}\nhelper_mode=body-interface-v2\n"
                    f"source_sha256={hashlib.sha256(source).hexdigest()}\n"
                    "trust_policy=selected_readonly_generation_no_concurrent_mutation\n")
                env["WITCHY_WASM_READY_BINDING"] = str(binding)
            session = client.Compiler(compiler, env, client.executable_identity(os.stat(compiler)))
            out = root / "module.wasm"
            with ThreadPoolExecutor(max_workers=1) as worker:
                pending = worker.submit(session.request, ["build", fixture, out, "4", "64", "16384"])
                try:
                    deadline = time.monotonic() + 2
                    while not pending.done() and time.monotonic() < deadline:
                        if marker.is_file() and marker.stat().st_size:
                            break
                        time.sleep(0.005)
                    self.assertTrue(marker.is_file() and marker.stat().st_size,
                                    "the proof child did not start")
                finally:
                    start = time.monotonic()
                    session.close()
                elapsed = time.monotonic() - start
                code, _, diagnostic, _ = pending.result()
            self.assertLess(elapsed, 2)
            self.assertEqual(code, 1)
            mode = "prepared" if prepared else "standalone"
            self.assertIn(f"{mode} Lean proof check cancelled", diagnostic)
            self.assertFalse(out.exists())
            self.assertEqual(list(scratch.iterdir()), [])
            with self.assertRaises(ProcessLookupError):
                os.kill(int(marker.read_text()), 0)


class ReviewedBuild(unittest.TestCase):
    def test_paths_resolve_against_the_callers_directory_when_named(self):
        """The entry is the one reviewed; the module lands where the caller named it, even if the
        process changes directory while the compiler builds."""
        witchy_compiler = load_cli().compiler_client()
        with tempfile.TemporaryDirectory() as temp, contextlib.chdir(temp):
            root = Path(temp).resolve()
            for name in ("review", "build", "elsewhere"):
                (root / name).mkdir()
            (root / "review/main.wy").write_text("fn main():\n    return\n")
            (root / "review/grants.toml").write_text("")
            requests = []

            class Session:
                def request(self, wire):
                    requests.append(list(wire))
                    if wire[0] == "prepare-grants":
                        return 0, "", "", "ticket"
                    if wire[0] == "build-checked":
                        Path(wire[3]).write_bytes(b"\0asm")
                        os.chdir(root / "elsewhere")  # another thread of the caller's moves
                        return 0, "", "", ""
                    return 0, "", "", ""
            with contextlib.chdir(root / "review"), mock.patch.object(witchy_compiler, "selected", lambda path, env: Session()):
                with witchy_compiler.reviewed_build("witchyc", "main.wy", "grants.toml", {}) as (review, build):
                    self.assertEqual(review.returncode, 0)
                    os.chdir(root / "build")
                    build("module.wasm")
            self.assertEqual(requests[0][1], str(root / "review/main.wy"))
            # No sizes named: the request is the one every compiler knows. Sizes named: they
            # go with the review (which keeps them for its build), never with the build.
            self.assertEqual(len(requests[0]), 4)
            with contextlib.chdir(root / "review"), mock.patch.object(witchy_compiler, "selected", lambda path, env: Session()):
                with witchy_compiler.reviewed_build("witchyc", "main.wy", "grants.toml", {}, sizes=(8, 128, 32768)) as (review, build):
                    build(str(root / "build/sized.wasm"))
            self.assertEqual(requests[-2][0], "prepare-grants")
            self.assertEqual(requests[-2][4:], ["8", "128", "32768"])
            self.assertEqual((requests[-1][:3], len(requests[-1])), (["build-checked", "ticket", str(root / "review/main.wy")], 4))
            self.assertEqual((root / "build/sized.wasm").read_bytes(), b"\0asm")
            checked = [wire for wire in requests if wire[0] == "build-checked"][0]
            self.assertEqual(checked[2], str(root / "review/main.wy"))
            self.assertEqual((root / "build/module.wasm").read_bytes(), b"\0asm")
            self.assertFalse((root / "elsewhere/module.wasm").exists())


if __name__ == "__main__":
    unittest.main()
