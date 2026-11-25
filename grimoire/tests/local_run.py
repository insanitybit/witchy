"""Integration checks using the configured witchyc and witchy-wasmtime-host."""

import contextlib
import hashlib
import json
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]


class LocalRun(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        shutil.copytree(ROOT / "examples/local-run", self.root, dirs_exist_ok=True)
        self.app = self.root / "app"
        self.manifest = self.app / "witchy.toml"

    def invoke(self, *args, cwd=None):
        from registry_run import load_cli, capture_children
        # CLI startup is covered by the signed registry flow. These checks still
        # compile and execute under the real grants from this manifest.
        command = ["run", *map(str, args)]
        with contextlib.chdir(cwd or self.root), capture_children() as (out, err):
            rc = load_cli().cli_main(command, {})
            out.seek(0)
            err.seek(0)
            return subprocess.CompletedProcess(command, rc, out.read(), err.read())

    def refused(self, message):
        result = self.invoke(self.manifest)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn(message, result.stderr)

    def test_run_reads_the_manifest_relative_config(self):
        (self.root / "settings.conf").write_text("1234\n")
        (self.root / "app.toml").symlink_to(self.manifest)
        for args, cwd, port in [([], self.app, 8080), (["app"], self.root, 443),
                                (["app.toml"], self.root, 9000)]:
            with self.subTest(args=args):
                (self.app / "settings.conf").write_text(f"{port}\n")
                result = self.invoke(*args, cwd=cwd)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, f"{port}\n")

    def test_missing_package(self):
        (self.app / "main.packages").write_text("")
        self.refused("main.packages lists no package of that name")

    def test_dependency_cannot_supply_the_missing_file_grant(self):
        package = self.root / "config-reader"
        (package / "witchy.toml").write_text('[grants]\ndocument = "reader.grants.toml"\n')
        (package / "reader.grants.toml").write_text('[files]\nconfig = "../app/settings.conf"\n')
        (self.app / "main.grants.toml").write_text("[files]\n")
        self.refused("the document grants none ([files].config)")

    def test_a_dependencys_tests_and_fixtures_are_never_read_built_or_run(self):
        """A package brings its source and nothing else: its `test.wy`, its fixtures, its
        manifest's `[[test]]`, `[tests]` and `[grants]` play no part in the consumer's build,
        review, run or test. Here they cannot even be compiled or opened, and nothing notices."""
        from registry_run import load_cli, capture_children
        package = self.root / "config-reader"
        (package / "test.wy").write_text('import "tests/steal.wy"\nthis is not Witchy and would refuse any build that read it\n')
        (package / "tests").mkdir()
        (package / "tests/steal.wy").write_text("test fn steal(home: &Dir<Write>, clock: &Clock) -> Result<(), Failure>:\n    return home.take_everything()\n")
        (package / "hostile").mkdir()
        (package / "hostile/main.wy").write_text("fn main(data: Dir):\n    return\n")
        (package / "hostile/witchy.toml").write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "all.grants.toml"\noutput = true\n')
        (package / "hostile/all.grants.toml").write_text(f'[dirs]\ndata = {{ root = "{self.app}", rights = ["Read", "Write"] }}\n')
        (package / "witchy.toml").write_text('[package]\nname = "config-reader"\nversion = "0.1.0"\nsource = "reader.wy"\n'
                                             '[tests]\ncase-deadline-ms = 4000\n[grants]\ndocument = "hostile/all.grants.toml"\noutput = true\nallocator = true\n'
                                             '[[test]]\nname = "steal"\nproject = "hostile"\nstdout = ""\n')
        os.chmod(package / "tests", 0)
        os.chmod(package / "hostile", 0)
        self.addCleanup(lambda: (os.chmod(package / "tests", 0o700), os.chmod(package / "hostile", 0o700)))
        self.manifest.write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "main.grants.toml"\noutput = true\n'
                                 '[[test]]\nname = "port"\nproject = "."\nstdout = "8080\\n"\n')
        before = sorted(str(p) for p in self.app.iterdir())
        def command(*args):
            with contextlib.chdir(self.root), capture_children() as (out, err):
                rc = load_cli().cli_main([*map(str, args)], {})
                out.seek(0); err.seek(0)
                return rc, out.read(), err.read()
        rc, out, err = command("run", self.app)
        self.assertEqual((rc, out), (0, "8080\n"), err)
        # Exactly the consumer's own grants, from its own document.
        self.assertEqual([line.removeprefix("grimoire: ") for line in err.splitlines() if line.startswith("grimoire: grant:")],
                         ["grant: output: the program writes its standard output", f"grant: file: {self.app.resolve() / 'settings.conf'}, read-only"])
        rc, out, err = command("build", self.app)
        self.assertEqual(rc, 0, err)
        rc, out, err = command("review", self.app)
        self.assertEqual(rc, 0, err)
        self.assertNotIn("steal", out + err)
        self.assertNotIn("Dir", out)
        rc, out, err = command("test", self.app)
        self.assertEqual((rc, out), (0, "ok port\n"), err)
        self.assertEqual(sorted(str(p) for p in self.app.iterdir() if p.name != "target"), before)

    def test_implicit_output_is_withheld(self):
        self.manifest.write_text(self.manifest.read_text().replace("output = true", "output = false"))
        self.refused("ungranted or invalid output import")

    def test_file_binding_cannot_be_used_for_writing(self):
        (self.app / "main.wy").write_text(
            "fn main(config: File<Write>):\n    config.raw_abandon()\n"
        )
        self.refused("main.grants.toml: the document grants what this launcher does not (the engine's --write-file)")
        self.assertEqual((self.app / "settings.conf").read_text(), "8080\n")

    def test_any_net_grant_is_refused_whatever_main_takes(self):
        """Grimoire grants no outbound network: a `[net]` section in the executable's document is
        refused before anything is read or built, even where `main` takes no Net (where the
        compiler would only warn of an over-grant and the app would run)."""
        document = self.app / "main.grants.toml"
        original = document.read_text()
        for name, section in [("granted", '[net]\nnet = ["127.0.0.1:9"]\n'), ("empty", "[net]\n"),
                              ("inline", 'net = { net = ["127.0.0.1:9"] }\n')]:
            with self.subTest(name):
                document.write_text(section + original if name == "inline" else original + section)
                self.refused("[net]: grimoire grants no outbound network access")
                self.assertFalse((self.app / "target").exists())

    def test_checkout_packages_need_the_executables_policy(self):
        (self.app / "main.policy").unlink()
        self.refused("main.policy is missing")

    def test_a_checkout_build_record_names_no_registry_digest(self):
        """A lock left beside a checkout build pins other copies: the record does not borrow them."""
        (self.app / "witchy.lock").write_text('[[package]]\nname = "config-reader"\nversion = "0.1.0"\n'
                                              f'digest = "sha384:{"0" * 96}"\nrequires = []\n')
        record = self.root / "build.json"
        result = self.invoke("--record", record, self.manifest)
        self.assertEqual(result.stdout, "8080\n", result.stderr)
        package = json.loads(record.read_text())["packages"]["config-reader"]
        reader = (self.root / "config-reader/reader.wy").read_bytes()
        self.assertEqual(package, {"source": "sha384:" + hashlib.sha384(reader).hexdigest(), "version": "0.1"})

    def test_standalone_compiler_checks_the_captured_grant_document(self):
        from registry_run import load_cli
        document = self.app / "main.grants.toml"
        original = document.read_bytes()
        record = self.root / "build.json"
        client = load_cli().compiler_client()
        reviewed_build = client.reviewed_build

        with patch.object(client, "selected", return_value=None):
            result = self.invoke("--record", record, self.manifest)
            self.assertEqual((result.returncode, result.stdout), (0, "8080\n"), result.stderr)
            self.assertEqual(json.loads(record.read_text())["grants"], "sha384:" + hashlib.sha384(original).hexdigest())
            record.unlink()

            @contextlib.contextmanager
            def replace_before_review(*args, **kwargs):
                document.write_bytes(original + b"# replacement before grant review\n")
                with reviewed_build(*args, **kwargs) as approved:
                    yield approved

            with patch.object(client, "reviewed_build", replace_before_review):
                result = self.invoke("--record", record, self.manifest)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")
            self.assertIn("grant document changed before review", result.stderr)
            self.assertFalse(record.exists())

    def test_wasm_review_keeps_the_grant_documents_relative_base(self):
        from registry_run import load_cli
        document = self.app / "main.grants.toml"
        original = document.read_bytes()
        for name in ("declared", "other"):
            folder = self.app / name
            folder.mkdir()
            (folder / document.name).write_bytes(original)
            (folder / "settings.conf").write_text("8080\n")
        document.unlink()
        first = self.app / "declared" / document.name
        second = self.app / "other" / document.name
        document.symlink_to(first)
        self.assertEqual(self.invoke(self.manifest).stdout, "8080\n")

        client = load_cli().compiler_client()
        reviewed_build = client.reviewed_build

        @contextlib.contextmanager
        def replace_before_review(*args, **kwargs):
            replacement = document.with_suffix(".new")
            replacement.symlink_to(second)
            replacement.replace(document)
            with reviewed_build(*args, **kwargs) as approved:
                yield approved

        for standalone in (False, True):
            with self.subTest(standalone=standalone), contextlib.ExitStack() as scope:
                document.unlink()
                document.symlink_to(first)
                if standalone:
                    scope.enter_context(patch.object(client, "selected", return_value=None))
                scope.enter_context(patch.object(client, "reviewed_build", replace_before_review))
                self.refused("grant document directory changed before review")
                self.assertEqual(document.read_bytes(), original)
                self.assertEqual(document.resolve(), second.resolve())

    def test_build_checks_module_size_before_replacing_the_artifact(self):
        from registry_run import load_cli, capture_children

        cli = load_cli()
        client = cli.compiler_client()
        original = self.manifest.read_text()
        output = self.root / "built.wasm"
        with capture_children():
            cli.build_project(self.app, output)
        wire = output.read_bytes()
        self.assertGreater(len(wire), 8)

        for standalone in (False, True):
            with self.subTest(standalone=standalone), contextlib.ExitStack() as scope:
                if standalone:
                    scope.enter_context(patch.object(client, "selected", return_value=None))
                scope.enter_context(capture_children())
                self.manifest.write_text(original.replace("[run]", f"[run]\nmodule-bytes = {len(wire) - 1}"))
                with self.assertRaisesRegex(OSError, "exceeds the module size limit"):
                    cli.build_project(self.app, output)
                self.assertEqual(output.read_bytes(), wire)
                output.unlink()
                with self.assertRaisesRegex(OSError, "exceeds the module size limit"):
                    cli.build_project(self.app, output)
                self.assertFalse(output.exists())
                self.manifest.write_text(original.replace("[run]", f"[run]\nmodule-bytes = {len(wire)}"))
                cli.build_project(self.app, output)
                self.assertEqual(output.read_bytes(), wire)

    def test_standalone_build_refuses_edits_to_reviewed_inputs(self):
        from registry_run import load_cli, capture_children
        cli = load_cli()
        client = cli.compiler_client()
        reviewed_build = client.reviewed_build
        output = self.root / "built.wasm"
        output.write_bytes(b"previous module")
        policy = self.app / "main.policy"
        # A checkout policy can constrain capabilities without pinning the source.
        policy.write_text(policy.read_text().split(" ; digest ", 1)[0] + "\n")

        for changed, suffix in ((self.root / "config-reader/reader.wy", b"\n// edited\n"),
                                (policy, b"\n# edited\n")):
            original = changed.read_bytes()

            @contextlib.contextmanager
            def edit_after_review(*args, **kwargs):
                with reviewed_build(*args, **kwargs) as reviewed:
                    self.assertEqual(reviewed[0].returncode, 0, reviewed[0].stderr)
                    changed.write_bytes(original + suffix)
                    yield reviewed

            try:
                with (self.subTest(file=changed.name), patch.object(client, "selected", return_value=None),
                      patch.object(client, "reviewed_build", edit_after_review), capture_children() as (_, err)):
                    with self.assertRaises(subprocess.CalledProcessError):
                        cli.build_project(self.app, output)
                    self.assertEqual(output.read_bytes(), b"previous module")
                    err.seek(0)
                    self.assertIn("compiler inputs changed after grant review", err.read())
            finally:
                changed.write_bytes(original)

    def test_review_accept_records_the_grant_bytes_that_were_checked(self):
        from registry_run import load_cli, capture_children
        cli = load_cli()
        document = self.app / "main.grants.toml"
        original = document.read_bytes()
        client = cli.compiler_client()
        reviewed_build = client.reviewed_build

        @contextlib.contextmanager
        def edit_on_release(*args, **kwargs):
            with reviewed_build(*args, **kwargs) as approved:
                yield approved
            document.write_bytes(original + b"# replacement after grant review\n")

        with (patch.object(client, "reviewed_build", edit_on_release),
              patch.object(cli, "record_reviewed", return_value="receipt") as accepted,
              contextlib.chdir(self.root), capture_children()):
            self.assertEqual(cli.review_project(self.manifest, accept=True), 0)
        receipt, = accepted.call_args.args
        self.assertIn(f"grants {len(original)}\n".encode() + original, receipt)
        self.assertNotEqual(document.read_bytes(), original)

    def test_record_keeps_the_grants_used_before_the_document_changes(self):
        from registry_run import load_cli
        grants = self.app / "main.grants.toml"
        reviewed = grants.read_bytes()
        record = self.root / "build.json"
        client = load_cli().compiler_client()
        reviewed_build = client.reviewed_build

        @contextlib.contextmanager
        def change_after_build(*args, **kwargs):
            with reviewed_build(*args, **kwargs) as (result, build):
                def change(output):
                    build(output)
                    grants.write_bytes(reviewed + b"# changed after build\n")
                yield result, change

        manifest_pin = "sha384:" + hashlib.sha384(self.manifest.read_bytes()).hexdigest()
        grant_pin = "sha384:" + hashlib.sha384(reviewed).hexdigest()
        with patch.object(client, "reviewed_build", change_after_build):
            result = self.invoke("--launch-pins", manifest_pin, grant_pin, "--record", record, self.manifest)
        self.assertEqual((result.returncode, result.stdout), (0, "8080\n"), result.stderr)
        self.assertNotEqual(grants.read_bytes(), reviewed)
        self.assertEqual(json.loads(record.read_text())["grants"], "sha384:" + hashlib.sha384(reviewed).hexdigest())

    def test_record_is_published_only_after_success(self):
        record = self.root / "run.json"
        original_manifest = self.manifest.read_text()
        self.manifest.write_text(original_manifest.replace('[run]', '[run]\noutput-bytes = 1'))
        failed = self.invoke("--record", record, self.manifest)
        self.assertNotEqual(failed.returncode, 0, failed.stderr)
        self.assertIn("witchy: limit reached: output", failed.stderr)
        self.assertFalse(record.exists())

        self.manifest.write_text(original_manifest)
        succeeded = self.invoke("--record", record, self.manifest)
        self.assertEqual((succeeded.returncode, succeeded.stdout), (0, "8080\n"), succeeded.stderr)
        completed = record.read_bytes()
        self.assertIs(json.loads(completed)["completed"], True)

        self.manifest.write_text(original_manifest.replace('[run]', '[run]\noutput-bytes = 1'))
        failed = self.invoke("--record", record, self.manifest)
        self.assertNotEqual(failed.returncode, 0, failed.stderr)
        self.assertEqual(record.read_bytes(), completed)

    def test_web_guest_refuses_changed_bytes_after_the_module_is_pinned(self):
        from registry_run import load_cli

        cli = load_cli()
        web = cli.web_builder()
        source = self.root / "guest.wy"
        wasm = self.root / "guest.wasm"
        compiler = cli.tool("WITCHYC", "witchyc")
        host = cli.tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")
        replacement = self.root / "replacement.wasm"
        for artifact, increment in ((wasm, 0), (replacement, 1)):
            source.write_text("import console\n\n"
                              "fn main(input: Console<Read>, output: Console<Write>):\n"
                              f"    output.print_byte(U8.truncate(input.next_byte() + {increment}))\n")
            built = subprocess.run([compiler, "build", source, "-o", artifact], capture_output=True, text=True)
            self.assertEqual(built.returncode, 0, built.stderr)
        other_wire = replacement.read_bytes()
        descriptor, wire = cli.open_compiled_module(wasm, web.MODULE_BYTES)
        self.addCleanup(os.close, descriptor)
        module = (descriptor, "sha384:" + web.hex_digest(wire))
        with wasm.open("r+b", buffering=0) as writer:
            wasm.unlink()
            limits = ("--input-bytes=1", "--output-bytes=1", "--fuel=1000000")
            self.assertEqual(web.run_guest(host, module, b"x", limits, 5), b"x")
            writer.write(other_wire)
            writer.truncate()
            with self.assertRaises(subprocess.CalledProcessError) as failure:
                web.run_guest(host, module, b"x", limits, 5)
            self.assertEqual(failure.exception.stdout, b"")
            replacement_module = (descriptor, "sha384:" + web.hex_digest(other_wire))
            self.assertEqual(web.run_guest(host, replacement_module, b"x", limits, 5), b"y")

    def test_record_and_run_keep_the_manifest_read_at_start(self):
        from registry_run import load_cli
        cli = load_cli()
        original = self.manifest.read_bytes()
        record = self.root / "build.json"
        read_project = cli.read_project

        def change_after_read(*args, **kwargs):
            result = read_project(*args, **kwargs)
            self.manifest.write_bytes(b'[run]\nentry = "main.wy"\ndeadline-ms = -1\n'
                                      b'[dependencies]\nmissing = "1.0.0"\n')
            return result

        manifest_pin = "sha384:" + hashlib.sha384(original).hexdigest()
        grant_pin = "sha384:" + hashlib.sha384((self.app / "main.grants.toml").read_bytes()).hexdigest()
        with patch.object(cli, "read_project", change_after_read), patch("registry_run.load_cli", return_value=cli):
            result = self.invoke("--launch-pins", manifest_pin, grant_pin, "--record", record, self.manifest)
        self.assertEqual((result.returncode, result.stdout), (0, "8080\n"), result.stderr)
        self.assertNotEqual(self.manifest.read_bytes(), original)
        self.assertEqual(json.loads(record.read_text())["manifest"], "sha384:" + hashlib.sha384(original).hexdigest())

    def test_a_main_that_takes_a_socket_is_launched_once_for_each_connection(self):
        """`main(connection: Socket)` and a `[listen]` grant: the launch is per connection (no
        flag or key of the project's asks for it), said in a `grant:` line, with the
        connection's deadline as a `limit:` line; each connection is answered by a new instance."""
        import socket
        with socket.socket() as free:
            free.bind(("127.0.0.1", 0))
            port = free.getsockname()[1]
        (self.app / "main.wy").write_text('import console\nimport net\n\nfn main(console: Console<Write>, connection: Socket):\n    let _ = connection.send("hi\\n")\n    connection.close()\n')
        (self.app / "main.packages").unlink()
        (self.app / "main.policy").unlink()
        (self.app / "main.grants.toml").write_text(f'[listen]\nconnection = "127.0.0.1:{port}"\n')
        self.manifest.write_text('[run]\nentry = "main.wy"\ndeadline-ms = 20000\nconnection-deadline-ms = 250\nfuel-per-connection = 50000\n[grants]\ndocument = "main.grants.toml"\noutput = true\n')
        server = subprocess.Popen([ROOT / "grimoire", "run", self.manifest], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        answers = []
        try:
            until = time.monotonic() + 15
            while len(answers) < 2 and time.monotonic() < until and server.poll() is None:
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=2) as client:
                        answers.append(client.makefile("rb").read())
                except OSError:
                    time.sleep(0.02)
        finally:
            server.terminate()
            said = server.communicate(timeout=10)[1]
        self.assertEqual(answers, [b"hi\n", b"hi\n"], said)
        self.assertIn(f"grimoire: grant: connections: accepts connections on 127.0.0.1:{port} (loopback only) and runs the program once for each, "
                      "in a new instance that keeps nothing from the last; dials nothing\n", said)
        self.assertIn("grimoire: limit: each connection: stops after 250 ms (that connection is closed; the server goes on)\n", said)
        self.assertIn("grimoire: limit: each connection: stops after 50000 instructions (that connection is closed; the server goes on)\n", said)

    def test_what_a_program_prints_cannot_pass_for_the_launchers_own_lines(self):
        """A line the program begins as the launcher begins its own is marked, unless the
        operator asks at this launch for the bytes as they are; the manifest cannot ask."""
        (self.app / "main.wy").write_text('import console\n\nfn main(console: Console<Write>):\n    console.line("grimoire: grant: secret everything")\n    console.line("witchy: limit reached: none")\n    console.line("grant: mine")\n    console.line("fine")\n')
        (self.app / "main.packages").unlink()
        (self.app / "main.policy").unlink()
        self.manifest.write_text('[run]\nentry = "main.wy"\n[grants]\noutput = true\n')
        result = self.invoke(self.app)
        self.assertEqual((result.returncode, result.stdout), (0, "| grimoire: grant: secret everything\n| witchy: limit reached: none\ngrant: mine\nfine\n"), result.stderr)
        # Every line of the launcher's and the engine's own begins with its one prefix.
        self.assertTrue(result.stderr and all(line.startswith(("grimoire: ", "witchy: ")) for line in result.stderr.splitlines()), result.stderr)
        raw = self.invoke("--raw-output", self.app)
        self.assertEqual((raw.returncode, raw.stdout), (0, "grimoire: grant: secret everything\nwitchy: limit reached: none\ngrant: mine\nfine\n"), raw.stderr)
        # What the compiler says of a source line is made plain whatever the flag: it is not the program's output.
        (self.app / "main.wy").write_text('fn main():\n    let x = undefined_thing  // \x1b[31mRED\x1b[0m\n    return\n')
        for flags in ((), ("--raw-output",)):
            broken = self.invoke(*flags, self.app)
            self.assertNotEqual(broken.returncode, 0)
            self.assertIn("// \\x1b[31mRED\\x1b[0m", broken.stderr)
            self.assertNotIn("\x1b", broken.stderr + broken.stdout)
        self.manifest.write_text('[run]\nentry = "main.wy"\nraw-output = true\n[grants]\noutput = true\n')
        self.refused("invalid [run].raw-output")

    def test_a_stated_write_right_lets_the_program_keep_a_file_in_its_own_directory(self):
        """`rights = ["Read", "Write"]` in the executable's document is a grant the launcher
        gives: shown as a line of its own before the run, and the file is there after it. The
        same directory holding a source of the project is refused before anything is built."""
        (self.app / "main.wy").write_text(
            "import console\nimport fs\nimport string\n\n"
            "fn main(console: Console<Write>, allocator: Allocator, data: Dir):\n"
            "    let Ok(made): String = String.new(&allocator) else Err(code):\n        return\n"
            "    var text = move made\n    let pushed = text.push_text(&allocator, \"kept\\n\")\n"
            "    match data.write_text(\"note.txt\", &text):\n        Ok:\n            console.line(\"written\")\n"
            "        Err(why):\n            console.line(\"not written\")\n    text.close()\n")
        (self.app / "main.packages").unlink()
        (self.app / "main.policy").unlink()
        (self.app / "data").mkdir()
        (self.app / "main.grants.toml").write_text('[dirs]\ndata = { root = "data", rights = ["Read", "Write"] }\n')
        self.manifest.write_text('[run]\nentry = "main.wy"\nfile-bytes = 4096\n[grants]\ndocument = "main.grants.toml"\noutput = true\nallocator = true\n')
        result = self.invoke(self.app, "--record", self.root / "record.json")
        self.assertEqual((result.returncode, result.stdout), (0, "written\n"), result.stderr)
        # Every granted directory is passed with what it is and held to data: read or written.
        import json
        examined = os.stat(self.app / "data")
        for rights in ('["Read", "Write"]', '["Read"]'):
            with self.subTest(rights=rights):
                if rights == '["Read"]':
                    source = (self.app / "main.wy").read_text()
                    (self.app / "main.wy").write_text('import console\nimport fs\n\nfn main(console: Console<Write>, data: Dir<Read>):\n    console.line("read")\n')
                    (self.app / "main.grants.toml").write_text('[dirs]\ndata = { root = "data", rights = ["Read"] }\n')
                    again = self.invoke(self.app, "--record", self.root / "record.json")
                    self.assertEqual((again.returncode, again.stdout), (0, "read\n"), again.stderr)
                    (self.app / "main.wy").write_text(source)
                    (self.app / "main.grants.toml").write_text('[dirs]\ndata = { root = "data", rights = ["Read", "Write"] }\n')
                flags = json.loads((self.root / "record.json").read_text())["host_flags"]
                at = flags.index(f"--dir={self.app.resolve() / 'data'}")
                self.assertIn(f"--dir-id={examined.st_dev}:{examined.st_ino}", flags[at:])
                self.assertIn("--data-only", flags[at:])
                self.assertIn("--no-links", flags[at:])
        self.assertEqual((self.app / "data/note.txt").read_text(), "kept\n")
        self.assertIn(f"grant: directory: {self.app.resolve() / 'data'}, read and write (no symbolic link followed, no other device entered)\n", result.stderr)
        self.assertIn("limit: files: 4096 bytes read and written in the whole run\n", result.stderr)
        # A name a build would read is never made there, nor anything hidden, by the engine
        # itself: whatever appears in the directory after the launcher looked, it stays data.
        for name in ("later.wy", ".hidden", "main.policy"):
            with self.subTest(name=name):
                source = (self.app / "main.wy").read_text()
                (self.app / "main.wy").write_text(source.replace('"note.txt"', f'"{name}"'))
                result = self.invoke(self.app)
                self.assertEqual((result.returncode, result.stdout), (0, "not written\n"), result.stderr)
                self.assertFalse((self.app / "data" / name).exists())
                (self.app / "main.wy").write_text(source)
        (self.app / "data/later.wy").write_text("pub fn one() -> U64:\n    return 1\n")
        self.refused("data/later.wy: a program is never granted write access to a manifest")

    def test_heap_needs_explicit_allocator_grant(self):
        (self.app / "main.wy").write_text("fn main(allocator: Allocator):\n    return\n")
        # A program of its own, with no packages: the example's package list and policy go.
        (self.app / "main.packages").unlink()
        (self.app / "main.policy").unlink()
        self.manifest.write_text('[run]\nentry = "main.wy"\n')
        self.refused("ungranted or invalid allocator import")
        with self.manifest.open("a") as stream:
            stream.write("[grants]\nallocator = true\n")
        result = self.invoke(self.app)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
