"""Exercise ordinary project artifacts, test results and their failure boundary."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class PublicCommands(unittest.TestCase):
    def run_cli(self, *argv):
        from registry_run import load_cli, capture_children
        from unittest import mock
        import sys
        cli = load_cli()
        # Registered under its name, as the launcher's own module is (`grimoire test` hands
        # itself to the native test runner by that name).
        with capture_children() as (out, err), mock.patch.dict(sys.modules, {cli.__name__: cli}):
            status = cli.cli_main(list(map(str, argv)), {})
            out.seek(0)
            err.seek(0)
            return subprocess.CompletedProcess(argv, status, out.read(), err.read())

    def test_build_and_test_the_declared_executable(self):
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / "main.wy").write_text("fn main(console: Console<Write>):\n    console.print_int(8080)\n")
            (project / "main.grants.toml").touch()
            manifest = project / "witchy.toml"
            manifest.write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "main.grants.toml"\noutput = true\n' + '\n[[test]]\nname = "port"\nproject = "."\nstdout = "8080\\n"\n')
            def run(*argv):
                return self.run_cli(*argv, project)
            self.assertEqual(run("build").returncode, 0)
            module = project / "target/main.wasm"
            self.assertEqual(module.read_bytes()[:4], b"\0asm")
            self.assertEqual(run("test").stdout, "ok port\n")
            manifest.write_text(manifest.read_text().replace('stdout = "8080', 'stdout = "8081'))
            self.assertNotEqual(run("test","--projects").returncode, 0)
            before = module.read_bytes()
            (project / "main.policy").write_text("main.wy: none\n")
            self.assertNotEqual(run("build").returncode, 0)
            self.assertEqual(module.read_bytes(), before)

    def test_library_build_does_not_pack_a_replaced_manifest(self):
        from registry_run import capture_children, load_cli
        from unittest import mock

        cli = load_cli()
        with tempfile.TemporaryDirectory() as directory:
            project = Path(directory).resolve()
            manifest, output = project / "witchy.toml", project / "library.coven"
            original = b'[package]\nname = "library"\nversion = "1.0.0"\nsource = "lib.wy"\n'
            manifest.write_bytes(original)
            (project / "lib.wy").write_text("pub fn answer() -> U64:\n    return 42\n")
            output.write_bytes(b"previous")
            run = subprocess.run

            def change_before_pack(command, *args, **kwargs):
                if command[1:2] == ["pack"]:
                    self.assertEqual(command[2:], [manifest, "--manifest-pin", cli.release_format().digest_of(original)])
                    manifest.write_bytes(original.replace(b"1.0.0", b"2.0.0"))
                return run(command, *args, **kwargs)

            with capture_children() as (_, err), mock.patch.object(cli.subprocess, "run", change_before_pack):
                with self.assertRaises(subprocess.CalledProcessError):
                    cli.build_project(project, output)
                err.seek(0)
                self.assertIn("package manifest differs from its expected SHA-384 pin", err.read())
            self.assertEqual(output.read_bytes(), b"previous")

    def test_launcher_refuses_special_and_oversized_control_files(self):
        with tempfile.TemporaryDirectory() as directory:
            project = Path(directory).resolve()
            (project / "main.wy").write_text("fn main():\n    return\n")
            files = {
                project / "witchy.toml": b'[run]\nentry = "main.wy"\n[grants]\ndocument = "app.grants.toml"\n',
                project / "witchy.lock": b"",
                project / "app.grants.toml": b"",
            }
            for path, wire in files.items():
                path.write_bytes(wire)
            command = [sys.executable, ROOT / "grimoire", "run", project]
            for path, wire in files.items():
                with self.subTest(path=path):
                    path.unlink()
                    os.mkfifo(path)
                    refused = subprocess.run(command, capture_output=True, timeout=5)
                    self.assertEqual((refused.returncode, refused.stdout), (1, b""))
                    self.assertIn(b"expected a regular control file", refused.stderr)
                    path.unlink()
                    with path.open("wb") as stream:
                        stream.truncate((32 << 20) + 1)
                    refused = subprocess.run(command, capture_output=True, timeout=5)
                    self.assertEqual((refused.returncode, refused.stdout), (1, b""))
                    self.assertIn(b"control file exceeds 33554432 bytes", refused.stderr)
                    path.write_bytes(wire)

    def test_compiler_refuses_a_nonregular_or_oversized_wasm_grant(self):
        compiler = os.environ.get("WITCHYC", "witchyc")
        with tempfile.TemporaryDirectory() as directory:
            project = Path(directory).resolve()
            entry = project / "main.wy"
            entry.write_text("fn main():\n    return\n")
            grant = project / "grant.toml"
            command = [compiler, "wasm-grants", entry, grant]
            os.mkfifo(grant)
            refused = subprocess.run(command, capture_output=True, timeout=5)
            self.assertEqual((refused.returncode, refused.stdout), (1, b""))
            self.assertIn(b"a grant document is a regular file", refused.stderr)
            grant.unlink()
            with grant.open("wb") as stream:
                stream.truncate((32 << 20) + 1)
            refused = subprocess.run(command, capture_output=True, timeout=5)
            self.assertEqual((refused.returncode, refused.stdout), (1, b""))
            self.assertIn(b"a grant document is at most 32768 KiB", refused.stderr)

    def test_new_projects_build_and_pass_and_a_missing_policy_can_be_written(self):
        import contextlib
        with tempfile.TemporaryDirectory() as tmp, contextlib.chdir(tmp):
            self.assertEqual(self.run_cli("new", "tide-table").returncode, 0)
            self.assertEqual(self.run_cli("build", "tide-table").returncode, 0)
            tested = self.run_cli("test", "tide-table")
            self.assertEqual((tested.returncode, tested.stdout), (0, "ok greets\n"), tested.stderr)
            self.assertEqual(self.run_cli("new", "tide-core", "--lib").returncode, 0)
            self.assertEqual(self.run_cli("build", "tide-core").returncode, 0)
            # Straight after `new`: `lock` has nothing to lock and says so (it ended in a traceback),
            # and `review --write-policy` drafts over the scaffold nobody reviewed.
            locked = self.run_cli("lock", "tide-table")
            self.assertEqual(locked.returncode, 1)
            self.assertIn("declares no [dependencies] and no [registry]: there is no package to lock", locked.stderr)
            self.assertNotIn("Traceback", locked.stderr)
            policy = Path("tide-table/main.policy")
            written = self.run_cli("review", "--write-policy", "tide-table")
            self.assertEqual(written.returncode, 0, written.stderr)
            self.assertIn("wrote tide-table/main.policy (in place of the one `grimoire new` wrote)", written.stdout)
            # The project's own file is held by what it may use, not by a digest of its text;
            # code the author did not write keeps its pin.
            lines = policy.read_text().splitlines()
            self.assertIn("main.wy: Console<Write> ; no unchecked", lines)
            standard = [line for line in lines if line.startswith("<standard>/")]
            self.assertTrue(standard and all("; digest sha384:" in line for line in standard), lines)
            self.assertEqual(self.run_cli("review", "tide-table").returncode, 0)
            # So the edit loop is one command: an edit that uses nothing new builds as it is.
            entry = Path("tide-table/main.wy")
            entry.write_text(entry.read_text() + "// an edit\n")
            self.assertEqual(self.run_cli("build", "tide-table").returncode, 0)
            # And one that reaches further is still refused by its line.
            entry.write_text("import console\n\nfn main(console: Console<Write>, allocator: Allocator):\n    console.line(\"hello from tide-table\")\n")
            refused = self.run_cli("build", "tide-table")
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn("Allocator", refused.stderr)
            # A policy that has been edited is replaced only when asked in so many words.
            before = policy.read_text()
            again = self.run_cli("review", "--write-policy", "tide-table")
            self.assertNotEqual(again.returncode, 0)
            self.assertIn("main.policy exists and has been edited", again.stderr)
            self.assertIn("--write-policy --replace", again.stderr)
            self.assertEqual(policy.read_text(), before)
            self.assertIn("--replace goes with --write-policy", self.run_cli("review", "--replace", "tide-table").stderr)
            replaced = self.run_cli("review", "--write-policy", "--replace", "tide-table")
            self.assertIn("in place of the policy that was there (--replace)", replaced.stdout)
            self.assertIn("main.wy: Allocator, Console<Write> ; no unchecked", policy.read_text().splitlines())
            # A missing policy is written as before.
            policy.unlink()
            self.assertEqual(self.run_cli("review", "tide-table").returncode, 1)
            self.assertFalse(policy.exists())
            self.assertIn("wrote tide-table/main.policy (a draft)", self.run_cli("review", "--write-policy", "tide-table").stdout)

    def test_a_new_service_is_run_once_for_each_connection_and_tests_its_answer(self):
        """`grimoire new NAME --service`: main takes a Socket and a directory to read and write,
        the document grants them, and the project's own native test checks its answer as a
        function of the request's bytes. It passes, and reviews, as written."""
        import contextlib
        with tempfile.TemporaryDirectory() as tmp, contextlib.chdir(tmp):
            self.assertEqual(self.run_cli("new", "tide-desk", "--service").returncode, 0)
            tested = self.run_cli("test", "tide-desk")
            self.assertEqual(tested.returncode, 0, tested.stderr)
            self.assertEqual(sorted(line.split()[1] for line in tested.stdout.splitlines() if line.startswith("pass ")),
                             ["test.wy::another_path_is_not_found", "test.wy::answers_the_root", "test.wy::anything_else_is_a_bad_request"], tested.stdout)
            reviewed = self.run_cli("review", "tide-desk")
            self.assertEqual(reviewed.returncode, 0, reviewed.stdout + reviewed.stderr)
            self.assertIn("grant: connections: accepts connections on 127.0.0.1:8080 (loopback only) and runs the program once for each", reviewed.stdout)
            self.assertRegex(reviewed.stdout, r"(?m)^grant: directory: .*/tide-desk/data, read and write")
            self.assertRegex(reviewed.stdout, r"(?m)^limit: no deadline")
            self.assertRegex(reviewed.stdout, r"(?m)^limit: each connection: stops after \d+ instructions")
            # Its scaffold policy is drafted over, as an executable's is.
            self.assertIn("in place of the one `grimoire new` wrote", self.run_cli("review", "--write-policy", "tide-desk").stdout)

    def test_a_new_tool_takes_an_argument_and_its_standard_input(self):
        """`grimoire new NAME --tool`: main takes its arguments and standard input, granted by
        its document, and its declared test gives it both."""
        import contextlib
        with tempfile.TemporaryDirectory() as tmp, contextlib.chdir(tmp):
            self.assertEqual(self.run_cli("new", "tide-pipe", "--tool").returncode, 0)
            tested = self.run_cli("test", "tide-pipe")
            self.assertEqual((tested.returncode, tested.stdout), (0, "ok prefixes\n"), tested.stderr)
            self.assertIn("grant: input: this launch's standard input, read as the program asks for it", tested.stderr)
            self.assertIn('grant: arguments: 1 given after `--`: "> "', tested.stderr)

    def test_a_quiet_run_says_one_line_for_the_grants_a_review_accepted(self):
        """`grimoire run --quiet`: one line in place of the `grant:` and `limit:` lines when a
        `grimoire review --accept` on this machine recorded exactly these for this manifest,
        grant document and lock. Without that record, or after any of the three changed, every
        line is shown. The engine's own last line is never left out."""
        import contextlib
        import os
        from unittest import mock
        with tempfile.TemporaryDirectory() as tmp, contextlib.chdir(tmp), mock.patch.dict(os.environ, XDG_STATE_HOME=str(Path(tmp) / "state")):
            self.assertEqual(self.run_cli("new", "tide-quiet").returncode, 0)
            project = Path("tide-quiet")
            def quiet_run():
                ran = self.run_cli("run", "--quiet", project)
                self.assertEqual((ran.returncode, ran.stdout), (0, "hello from tide-quiet\n"), ran.stderr)
                self.assertRegex(ran.stderr, r"(?m)^witchy: limit: used: ")
                return [line for line in ran.stderr.splitlines() if line.startswith("grimoire: ")]
            def shown_whole(said):
                self.assertTrue(said[0].startswith("grimoire: not quiet: "), said)
                self.assertIn("grimoire: grant: output: the program writes its standard output", said)
                self.assertTrue(any(line.startswith("grimoire: limit: ") for line in said), said)
            shown_whole(quiet_run())
            accepted = self.run_cli("review", "--accept", project)
            self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)
            self.assertIn("recorded these grants as reviewed on this machine", accepted.stdout)
            said = quiet_run()
            self.assertEqual(len(said), 1, said)
            self.assertRegex(said[0], r"^grimoire: grants as reviewed \(digest [0-9a-f]{96}\)$")
            manifest, document = project / "witchy.toml", project / "main.grants.toml"
            original = manifest.read_text()
            manifest.write_text(original.replace("# fuel = 1000000", "fuel = 2000000"))
            said = quiet_run()
            shown_whole(said)
            self.assertIn("grimoire: limit: stops after 2000000 instructions", said)
            manifest.write_text(original)
            self.assertEqual(len(quiet_run()), 1)
            document.write_text(document.read_text() + "# reviewed again?\n")
            shown_whole(quiet_run())
            document.write_text(document.read_text().replace("# reviewed again?\n", ""))
            self.assertEqual(len(quiet_run()), 1)
            self.assertEqual(self.run_cli("lock", "--toolchain", project).returncode, 0)
            shown_whole(quiet_run())

    def test_a_tool_takes_the_words_after_the_two_dashes_and_nothing_else_does(self):
        """`main(args: Args)`, granted by `[args]`: the words after `--` reach the program as
        they were typed, are shown escaped on one `grant:` line, and are never an option of
        the launcher or the engine. A `[[test]]` gives them as `args`."""
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / "main.wy").write_text("import args\nimport console\n\nfn main(console: Console<Write>, args: Args, allocator: Allocator):\n"
                                             "    for index in 0..args.count():\n        match args.get(&allocator, index):\n            Ok(word):\n"
                                             "                console.write_string(&word)\n                console.line(\"\")\n                word.close()\n"
                                             "            Err(error):\n                console.line(\"missing\")\n")
            (project / "main.grants.toml").write_text("[args]\nargs = { count = 3, max-bytes = 48 }\n")
            manifest = project / "witchy.toml"
            manifest.write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "main.grants.toml"\noutput = true\nallocator = true\n'
                                '\n[[test]]\nname = "echoes"\nproject = "."\nargs = ["a b", "--raw-output", "grant: all\\nlimit: none"]\nstdout = "a b\\n--raw-output\\ngrant: all\\nlimit: none\\n"\n')
            tested = self.run_cli("test", project)
            self.assertEqual((tested.returncode, tested.stdout), (0, "ok echoes\n"), tested.stderr)
            self.assertIn('grimoire: grant: arguments: 3 given after `--`: "a b" "--raw-output" "grant: all\\nlimit: none"\n'
                          "grimoire: limit: arguments: at most 3 words, 48 bytes in all; a launch with more is refused\n", tested.stderr)
            # Not raw: the program's own line that begins as a launcher's is still marked.
            shown = self.run_cli("run", project, "--", "grimoire: all")
            self.assertEqual((shown.returncode, shown.stdout), (0, "| grimoire: all\n"), shown.stderr)
            # The engine's last line is what the run used of each limit, beside the key that sets it.
            self.assertRegex(shown.stderr, r"(?m)^witchy: limit: used: instructions \d+ of 1000000 \(\[run\] fuel\); output 14 of 65536 bytes \(\[run\] output-bytes\); memory \d+ of 4 pages$")
            # More than granted is refused before anything runs.
            over = self.run_cli("run", project, "--", "1", "2", "3", "4")
            self.assertEqual((over.returncode != 0, over.stdout), (True, ""))
            self.assertIn("this launch gives 4 arguments of 4 bytes after `--`, and the document's [args] grants at most 3 words and 48 bytes", over.stderr)

    def test_a_pure_tool_takes_its_input_from_the_pipe_or_a_file_the_document_names(self):
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / "main.wy").write_text("import console\n\nfn main(console: Console<Write>, input: Console<Read>, allocator: Allocator):\n    var total: U64 = 0\n"
                                             "    while true:\n        let byte = input.next_byte()\n        if byte == 256:\n            break\n        total += byte\n    console.print_int(total)\n")
            (project / "main.grants.toml").write_text("[input]\ninput = { stdin = true }\n")
            manifest = project / "witchy.toml"
            manifest.write_text('[run]\nentry = "main.wy"\ninput-bytes = 3\n[grants]\ndocument = "main.grants.toml"\noutput = true\nallocator = true\n'
                                '\n[[test]]\nname = "sums"\nproject = "."\nstdin = "AB"\nstdout = "131\\n"\n')
            tested = self.run_cli("test", project)
            self.assertEqual((tested.returncode, tested.stdout), (0, "ok sums\n"), tested.stderr)
            self.assertIn("grant: input: this launch's standard input, read as the program asks for it", tested.stderr)
            self.assertIn("limit: input: 3 bytes in all; the read past them ends the run", tested.stderr)
            # More than its limit ends the run at the read past it, and says which key to raise.
            manifest.write_text(manifest.read_text().replace('stdin = "AB"', 'stdin = "ABCD"'))
            over = self.run_cli("test", project)
            self.assertNotEqual(over.returncode, 0)
            self.assertIn('input exceeds its grant: more than 3 bytes on standard input: raise [run] input-bytes (a number of bytes, or "unlimited")', over.stderr)
            # A file the document names; a case with no `stdin` has an empty standard input.
            (project / "requests.txt").write_text("AC")
            (project / "main.grants.toml").write_text('[input]\ninput = "requests.txt"\n')
            manifest.write_text(manifest.read_text().replace('stdin = "ABCD"\n', '').replace("131", "132"))
            filed = self.run_cli("test", project)
            self.assertEqual((filed.returncode, filed.stdout), (0, "ok sums\n"), filed.stderr)
            self.assertIn(f"grant: input: {(project / 'requests.txt').resolve()}, read whole before the program starts", filed.stderr)
            (project / "main.grants.toml").write_text('[input]\ninput = true\n')
            self.assertIn("or the launcher's standard input (`input = { stdin = true }`)", self.run_cli("build", project).stderr)

    def test_no_command_ends_in_a_traceback(self):
        """An exception no command expected is one line and a failing status, with the
        traceback only when the operator asked for it at that launch."""
        from unittest import mock
        from registry_run import load_cli, capture_children
        import contextlib
        cli = load_cli()
        def run(*argv):
            with capture_children() as (out, err):
                status = cli.cli_main(list(argv), {})
                err.seek(0)
                return status, err.read()
        with tempfile.TemporaryDirectory() as tmp, contextlib.chdir(tmp):
            with mock.patch.object(cli, "lock_project", side_effect=AttributeError("'NoneType' object has no attribute 'trust_first_use'")):
                status, said = run("lock")
                self.assertEqual(status, 70)
                self.assertEqual(said.count("\n"), 1, said)
                self.assertIn("grimoire: stopped by a defect in grimoire itself (AttributeError: 'NoneType' object has no attribute 'trust_first_use'); nothing further was done.", said)
                self.assertIn("GRIMOIRE_DEBUG=1", said)
                self.assertNotIn("Traceback", said)
                with mock.patch.object(cli, "DEBUG_TRACEBACK", True):
                    status, said = run("lock")
                self.assertEqual(status, 70)
                self.assertIn("Traceback", said)
            for command, function in (("build", "build_project"), ("review", "review_project"), ("run", "run_project"), ("new", "new_project"), ("publish", "publish_project")):
                with self.subTest(command=command), mock.patch.object(cli, function, side_effect=RuntimeError("boom\nwith a second line")):
                    status, said = run(command, *(["x"] if command == "new" else ["--key", "k"] if command == "publish" else []))
                    self.assertEqual((status, said.count("\n")), (70, 1), said)
                    self.assertIn("(RuntimeError: boom with a second line)", said)
        # A long silent step says it is still working; a short one says nothing.
        import io, threading
        said, say = threading.Event(), cli.say
        with contextlib.redirect_stderr(io.StringIO()) as shown, mock.patch.object(cli, "say", lambda text: (say(text), said.set())):
            with cli.still_working("deriving the key", after=0.01):
                self.assertTrue(said.wait(10), "a step silent past `after` says it is still working")
            with cli.still_working("never said", after=5):
                pass
        self.assertEqual(shown.getvalue(), "grimoire: deriving the key (still working: a first use can take half a minute)\n")
        # A failed check says which check and both values (reasons 4 to 7 had no name).
        describe = cli.native_tests().describe_failure
        self.assertEqual(describe(1, 41, 42), ("u64_mismatch", "eq_u64: got 41, expected 42"))
        self.assertEqual(describe(2, 1, 0), ("bool_mismatch", "eq_bool: got true, expected false"))
        self.assertEqual(describe(3, 0, 1), ("condition", "ok: the condition was false"))
        self.assertEqual(describe(4, (1 << 64) - 3, 5), ("i64_mismatch", "eq_i64: got -3, expected 5"))
        self.assertEqual(describe(5, 2, 65), ("byte_mismatch", "eq_bytes: the byte at index 2 is not the expected 65 (0x41)"))
        self.assertEqual(describe(6, 3, 6), ("length_mismatch", "eq_bytes: got 3 bytes, expected 6; one is the start of the other"))
        self.assertEqual(describe(7, 1, 0), ("set_up_failed", "set_up: the step answered Err(1)"))
        self.assertEqual(describe(9, 1, 2), ("assertion", "reason 9: actual 1, expected 2"))
        # The draft's pins: none on the project's own files, kept on a package's and the library's.
        digest = "sha384:" + "a" * 96
        draft = f"# c\n<standard>/wasm-vec.wy: Allocator ; digest {digest}\n@core: nothing ; no unchecked ; digest {digest}\n@core/part.wy: nothing ; digest {digest}\nmain.wy: Console<Write> ; no unchecked ; digest {digest}\nlib/eval.wy: nothing ; digest {digest}  # note\n"
        self.assertEqual(cli.own_files_unpinned(draft), f"# c\n<standard>/wasm-vec.wy: Allocator ; digest {digest}\n@core: nothing ; no unchecked ; digest {digest}\n@core/part.wy: nothing ; digest {digest}\nmain.wy: Console<Write> ; no unchecked\nlib/eval.wy: nothing  # note\n")

    def test_library_artifact_contains_the_checked_source_and_release_record(self):
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "reader"
            shutil.copytree(ROOT / "examples/local-run/config-reader", project)
            cmd = ["build", project]
            self.run_cli(*cmd).check_returncode()
            module = project / "target/config-reader-0.1.0.coven"
            source = project / "reader.wy"
            expected = b"witchy-package 1\nname config-reader\nversion 0.1.0\n\n" + source.read_bytes()
            self.assertEqual(module.read_bytes(), expected)
            source.write_text("this is not a Witchy program\n")
            refused = self.run_cli(*cmd)
            self.assertNotEqual(refused.returncode, 0)
            # The diagnostic names the author's file, not the copy of the release it was checked from.
            self.assertIn(f"{source.resolve()}:1:", refused.stderr)
            self.assertNotIn("grimoire-library-", refused.stderr)
            self.assertEqual(module.read_bytes(), expected)


if __name__ == "__main__":
    unittest.main()
