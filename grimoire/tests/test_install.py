"""install.py: every module a tool loads is installed with it, the prefix is the user's own,
and nothing is written through a link (the third dogfooding pass found an install that missed
witchy_release.py and the engine's crypto library)."""

import ast
from hashlib import sha384
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("grimoire_install", ROOT / "install.py")
install = importlib.util.module_from_spec(spec)
spec.loader.exec_module(install)


def coven_entry():
    """The Coven the suite runs with (`COVEN`), when it is a source checkout's."""
    named = os.environ.get("COVEN")
    path = Path(shutil.which(named) or named).resolve() if named else None
    return path if path is not None and path.name == "coven" and (path.parent / "coven_http.py").is_file() else None


_parsed = {}


def parsed(source):
    """`ast.parse(source)`, once for each text: the installed copies are the checkout's bytes."""
    if source not in _parsed:
        _parsed[source] = ast.parse(source)
    return _parsed[source]


class Install(unittest.TestCase):
    def setUp(self):
        # The policy always allows /tmp, independent of the caller's TMPDIR.
        self.root = Path(tempfile.mkdtemp(prefix="grimoire-install-test-", dir="/tmp")).resolve()
        self.addCleanup(shutil.rmtree, self.root)

    def loaded(self, directory):
        """Every module the Python in `directory` imports or names as a file, and whether each
        is the standard library's or in the directory."""
        missing = []
        for source in sorted(directory.iterdir()):
            if source.suffix not in (".py", "") or source.is_dir():
                continue
            tree = parsed(source.read_bytes())
            for node in ast.walk(tree):
                names = []
                if isinstance(node, ast.Import):
                    names = [alias.name.partition(".")[0] for alias in node.names]
                elif isinstance(node, ast.ImportFrom) and node.module:
                    names = [node.module.partition(".")[0]]
                for name in names:
                    if name not in sys.stdlib_module_names and not (directory / f"{name}.py").is_file():
                        missing.append(f"{source.name} imports {name}")
                if isinstance(node, ast.Constant) and isinstance(node.value, str) and re.fullmatch(r"\w+\.py", node.value):
                    if not (directory / node.value).is_file():
                        missing.append(f"{source.name} names {node.value}")
        return missing

    def test_every_module_a_tool_loads_is_installed_with_it(self):
        prefix = install.Prefix(self.root / "prefix")
        coven = coven_entry()
        tools = [("grimoire", ROOT / "grimoire")] + ([("coven", coven)] if coven else [])
        names = {}
        for name, entry in tools:
            files = install.tool_files(entry, install.COVEN_DATA if name == "coven" else install.GRIMOIRE_DATA)
            names[name] = {filename for filename, _, mode in files if mode == 0o644 and filename.endswith(".py")}
            for filename, source, mode in files:
                prefix.write(Path("libexec") / name / filename, source.read_bytes(), mode)
            prefix.link(Path("bin") / name, f"../libexec/{name}/{entry.name}")
            installed = self.root / "prefix" / "libexec" / name
            self.assertEqual(self.loaded(installed), [], name)
            # Through its link, with nothing of the checkout on the import path.
            help_command = [sys.executable, "-I", self.root / "prefix" / "bin" / name, "--help"]
            ran = subprocess.run(help_command, capture_output=True, text=True, cwd=self.root)
            self.assertEqual(ran.returncode, 0, ran.stderr)
            if name == "coven":
                packages = {
                    "oidc": {"oidc.wy", "compact.wy", "jwk_fields.wy", "rs256_keys.wy"},
                    "webauthn": {"webauthn.wy", "cose.wy"},
                }
                for package, members in packages.items():
                    relative = Path("vendor") / package
                    self.assertEqual({source.name for source in (installed / relative).iterdir()}, members)
                    for filename in members:
                        self.assertEqual((installed / relative / filename).read_bytes(),
                                         (entry.parent / relative / filename).read_bytes())
            if name == "grimoire":
                for asset in ("glamour-browser.mjs", "glamour-worker.mjs", "glamour-frame.html", "coven.css"):
                    self.assertEqual((installed / asset).read_bytes(), (entry.parent / asset).read_bytes())
                checked = ("grimoire_cli.py", "grimoire_limits.py", "glamour_web.py")
            else:
                checked = ("coven_cli.py",)
            for filename in checked:
                companion = installed / filename
                original = companion.read_bytes()
                with self.subTest(tool=name, companion=filename):
                    try:
                        companion.write_bytes(original + b"\n# changed after install\n")
                        command = help_command[:-1] + ["web", "build"] if filename == "glamour_web.py" else help_command
                        refused = subprocess.run(command, capture_output=True, text=True, cwd=self.root)
                        self.assertNotEqual(refused.returncode, 0)
                        self.assertEqual(refused.stdout, "")
                        self.assertIn(filename, refused.stderr)
                        self.assertIn("shipped with this launcher", refused.stderr)
                    finally:
                        companion.write_bytes(original)
        # What `tool_files` installed beside each entry (its `companions`).
        self.assertTrue({"witchy_compiler.py", "witchy_release.py", "package_tests.py", "test_sources.py", "property_trials.py"} <= names["grimoire"], names)
        self.assertTrue({"glamour_web.py", "glamour_document.py", "coven_catalog_web.py"} <= names["grimoire"], names)
        if coven:
            self.assertTrue({"coven_http.py", "witchy_compiler.py", "witchy_release.py"} <= names["coven"], names)
            if (coven.parent / "staging_schema.packages").is_file():
                installed = self.root / "prefix" / "libexec" / "coven"
                for name in ("staging_schema.packages", "vendor/sqlite/sqlite.wy", "review/snapshot.wy",
                             "response_fields.wy", "https_reply.wy",
                             "legacy_http_reply.wy", "legacy_http_reply.policy",
                             "mirror/identity.wy", "mirror/paths.wy", "mirror/routes.wy",
                             "mirror/web_assets.wy", "mirror/web_headers.wy", "mirror/web_response.wy",
                             "mirror/web_routes.wy", "web/render.wy", "web/render.grants.toml",
                             "web/browser/filter.wy", "web/browser/filter.grants.toml"):
                    self.assertEqual((installed / name).read_bytes(), (coven.parent / name).read_bytes())
                    self.assertFalse((installed / name).is_symlink())

    def test_client_bundle_requires_its_pin_and_validates_all_paths_before_install(self):
        archive = self.root / "client.zip"
        prefix = self.root / "installed"
        required = sorted(install.BUNDLE_EXECUTABLES)
        if sys.platform == "darwin":
            required.append("bin/libaws_lc_fips_0_14_4_crypto.dylib")

        def bundle(extra=None):
            with zipfile.ZipFile(archive, "w") as wire:
                wire.comment = install.BUNDLE_ID
                for name in required:
                    wire.writestr(name, name.encode())
                if extra is not None:
                    wire.writestr(extra, b"outside")
            return "sha384:" + sha384(archive.read_bytes()).hexdigest()

        link = zipfile.ZipInfo("libexec/coven/linked.wy")
        link.external_attr = 0o120777 << 16
        flags = ["--from-bundle", str(archive), "--prefix", str(prefix), "--no-self-check", "--bundle-pin"]
        with patch.object(install, "mbx_build", side_effect=AssertionError("attempted a build")), \
             patch.object(install, "check_engine", return_value="engine starts"):
            for invalid in ("libexec/coven/../../../outside", link, "libexec/coven/coven/child",
                            install.TUF_BUNDLE_DIR + "tuf_get.wasm", install.MANIFEST_MODULE, install.MANIFEST_PIN):
                with self.subTest(invalid=invalid):
                    self.assertEqual(install.main([*flags, bundle(invalid)]), 1)
                    self.assertFalse(prefix.exists())
            pin = bundle()
            self.assertEqual(install.main([*flags, "sha384:" + "0" * 96]), 1)
            self.assertFalse(prefix.exists())
            with patch.object(install, "check_engine", side_effect=install.InstallError("worker failed")):
                self.assertEqual(install.main([*flags, pin]), 1)
            self.assertFalse(prefix.exists())
            self.assertEqual(list(self.root.glob(".witchy-install-*")), [])

            proof_flags = [flag for flag in flags if flag != "--no-self-check"]
            with patch.object(install, "self_check", side_effect=install.InstallError("proof failed")):
                self.assertEqual(install.main([*proof_flags, pin]), 1)
            self.assertFalse(prefix.exists())
            self.assertEqual(list(self.root.glob(".witchy-install-*")), [])

            def extra_companion(staged):
                (staged / "libexec/coven/unlisted.py").write_text("# unlisted")
                return "engine starts"

            with patch.object(install, "check_engine", side_effect=extra_companion):
                self.assertEqual(install.main([*flags, pin]), 1)
            self.assertFalse(prefix.exists())
            self.assertEqual(list(self.root.glob(".witchy-install-*")), [])

            def competing_prefix(staged):
                self.assertNotEqual(staged, prefix)
                self.assertTrue((staged / "bin/coven").is_file())
                self.assertFalse(prefix.exists())
                prefix.mkdir()
                return "engine starts"

            with patch.object(install, "check_engine", side_effect=competing_prefix):
                self.assertEqual(install.main([*flags, pin]), 1)
            self.assertEqual(list(prefix.iterdir()), [])
            self.assertEqual(list(self.root.glob(".witchy-install-*")), [])
            prefix.rmdir()
            other = self.root / "dangling"
            prefix.symlink_to(other)
            self.assertEqual(install.main([*flags, pin]), 1)
            self.assertTrue(prefix.is_symlink())
            self.assertFalse(other.exists())
            prefix.unlink()
            self.assertEqual(install.main([*flags, pin]), 0)
            self.assertEqual((prefix / "bin/coven").read_bytes(), b"libexec/coven/coven")
            self.assertEqual(install.main([*flags, pin]), 1)
            self.assertEqual((prefix / "bin/witchyc").read_bytes(), b"bin/witchyc")

            verify = ["--verify-bundle", str(archive), "--bundle-pin", pin, "--prefix"]
            with patch.object(install, "check_engine", side_effect=AssertionError("executed an installed tool")):
                missing = self.root / "missing"
                self.assertEqual(install.main([*verify, str(missing)]), 1)
                self.assertFalse(missing.exists())
                self.assertEqual(install.main([*verify, str(prefix)]), 0)
                executable = prefix / "libexec/coven/coven"
                original = executable.read_bytes()
                for relative in ("libexec/coven/extra.py", "bin/extra", "extra"):
                    extra = prefix / relative
                    extra.write_bytes(b"unlisted")
                    self.assertEqual(install.main([*verify, str(prefix)]), 1)
                    self.assertEqual(extra.read_bytes(), b"unlisted")
                    extra.unlink()
                cache = executable.parent / "__pycache__"
                cache.mkdir()
                self.assertEqual(install.main([*verify, str(prefix)]), 1)
                cache.rmdir()
                alias = self.root / "alias"
                os.link(executable, alias)
                self.assertEqual(install.main([*verify, str(prefix)]), 1)
                alias.unlink()
                self.assertEqual(install.main([*verify, str(prefix)]), 0)
                executable.write_bytes(original[::-1])
                self.assertEqual(install.main([*verify, str(prefix)]), 1)
                executable.write_bytes(original)
                entry = prefix / "bin/coven"
                entry.unlink()
                entry.symlink_to("../libexec/grimoire/grimoire")
                self.assertEqual(install.main([*verify, str(prefix)]), 1)
                self.assertEqual(os.readlink(entry), "../libexec/grimoire/grimoire")

    def test_bundle_pins_the_worker_bytes_it_installs(self):
        folder = self.root / "workers"
        folder.mkdir()
        body = b"\0asm\x01\0\0\0"
        for name in install.TUF_MODULES:
            (folder / f"{name}.wasm").write_bytes(body)
        workers = install.bundled_workers(folder)
        pin = "sha384:" + sha384(body).hexdigest()
        self.assertEqual(json.loads(workers.pop("pins.json")), dict.fromkeys(install.TUF_MODULES, pin))
        self.assertEqual(workers, dict.fromkeys((f"{name}.wasm" for name in install.TUF_MODULES), body))
        (folder / "tuf_get.wasm").write_bytes(b"invalid module")
        with self.assertRaisesRegex(install.InstallError, "invalid TUF worker module"):
            install.bundled_workers(folder)

    def test_bundled_tuf_check_runs_each_pinned_worker_without_registry_access(self):
        prefix = self.root / "installed"
        host = prefix / "bin/witchy-wasmtime-host"
        with patch.object(install.subprocess, "run") as run:
            self.assertFalse(install.check_bundled_tuf_workers(prefix, host))
            run.assert_not_called()

            folder = prefix / install.TUF_BUNDLE_DIR
            folder.mkdir(parents=True)
            pins = {name: "sha384:" + str(index) * 96 for index, name in enumerate(install.TUF_MODULES)}
            pins_file = folder / "pins.json"
            bad_pins = (json.dumps(pins | {"tuf_get": "invalid"}), '{"tuf_get":"duplicate",' + json.dumps(pins)[1:])
            for invalid in bad_pins:
                pins_file.write_text(invalid)
                with self.assertRaisesRegex(install.InstallError, "invalid bundled TUF worker pins"):
                    install.check_bundled_tuf_workers(prefix, host)
            run.assert_not_called()
            pins_file.write_text(json.dumps(pins))
            refusals = (b"refused\n", b"refused 5\n", b"refused 2\n", b"refused lock\n")
            replies = [subprocess.CompletedProcess([], 0, refusal, b"") for refusal in refusals]
            run.side_effect = replies
            self.assertTrue(install.check_bundled_tuf_workers(prefix, host))
            self.assertEqual(run.call_count, len(pins))
            for name, invocation in zip(pins, run.call_args_list, strict=True):
                command = invocation.args[0]
                self.assertEqual(command[:3], [host, folder / f"{name}.wasm", f"--module-pin={pins[name]}"])
                network = [flag for flag in command[3:] if flag.startswith("--net")]
                self.assertEqual(network, ["--net=127.0.0.1:0", "--net-bytes=0"] if name.startswith("tuf_get") else [])
                self.assertEqual(invocation.kwargs["env"], {})
                self.assertEqual(invocation.kwargs["input"], b"{}\n")

            run.side_effect = [*replies[:-1], subprocess.CompletedProcess([], 1, b"", b"bad pin")]
            with self.assertRaisesRegex(install.InstallError, "could not check the bundled TUF worker tuf_extract"):
                install.check_bundled_tuf_workers(prefix, host)

    def test_manifest_worker_check_uses_the_pinned_parser(self):
        prefix = self.root / "installed"
        host = prefix / "bin/witchy-wasmtime-host"
        worker = prefix / install.MANIFEST_MODULE
        pin_file = prefix / install.MANIFEST_PIN
        with patch.object(install.subprocess, "run") as run:
            self.assertFalse(install.check_bundled_manifest_worker(prefix, host))
            worker.parent.mkdir(parents=True)
            worker.touch()
            pin_file.write_bytes(b"no pin")
            with self.assertRaisesRegex(install.InstallError, "invalid bundled package manifest worker pin"):
                install.check_bundled_manifest_worker(prefix, host)
            run.assert_not_called()
            pin = "sha384:" + "1" * 96
            pin_file.write_text(pin + "\n")

            def check(command, **options):
                self.assertEqual(command[:3], [host, worker, f"--module-pin={pin}"])
                self.assertEqual(options["env"], {})
                self.assertEqual(command[-1], f"--lifeline=fd:{options['pass_fds'][0]}")
                self.assertFalse(any(flag.startswith(("--net", "--write", "--dir")) for flag in command[3:]))
                wire = Path(command[3].removeprefix("--file=")).read_bytes()
                receipt = b"ok\n" + sha384(wire).digest() + b"5\nprobe5\n1.0.08\nprobe.wyend\n"
                return subprocess.CompletedProcess(command, 0, receipt, b"")

            run.side_effect = check
            self.assertTrue(install.check_bundled_manifest_worker(prefix, host))
            run.side_effect = None
            run.return_value = subprocess.CompletedProcess([], 0, b"ok\n", b"")
            with self.assertRaisesRegex(install.InstallError, "could not check the bundled package manifest worker"):
                install.check_bundled_manifest_worker(prefix, host)

    def test_a_module_the_source_loads_that_is_not_there_stops_the_install(self):
        tool = self.root / "tool"
        tool.mkdir()
        entry = tool / "entry"
        (tool / "helper.py").write_text("import third\n")
        (tool / "third.py").write_text("LOADED = 'helped.py'\n")
        (tool / "helped.py").write_text("")
        (tool / "unused.py").write_text("")
        entry.write_text("import os\nimport helper\n\ndef later():\n    return load('named.py'), {'by_name': 1}\n")
        (tool / "named.py").write_text("")
        (tool / "by_name.py").write_text("")
        self.assertEqual([path.name for path in install.companions(entry)], ["by_name.py", "helped.py", "helper.py", "named.py", "third.py"])
        for missing, said in (("third.py", "imports third, which is neither the standard library's nor beside it"),
                              ("helped.py", "names helped.py, which is not beside it"),
                              ("named.py", "names named.py, which is not beside it")):
            kept = (tool / missing).read_bytes()
            (tool / missing).unlink()
            with self.subTest(missing=missing), self.assertRaisesRegex(install.InstallError, said):
                install.companions(entry)
            (tool / missing).write_bytes(kept)
        entry.write_text("import not_a_module_anywhere\n")
        with self.assertRaisesRegex(install.InstallError, "imports not_a_module_anywhere"):
            install.companions(entry)

    def test_the_prefix_is_the_users_own_and_no_link_is_followed(self):
        for prefix in ("/usr/local", "/", "/opt/witchy", str(Path.home()), "relative/path"):
            with self.subTest(prefix=prefix), self.assertRaises(install.InstallError):
                install.checked_prefix(prefix)
        away = self.root / "away"
        away.mkdir()
        (self.root / "to-usr").symlink_to("/usr/local")
        with self.assertRaisesRegex(install.InstallError, "not inside the home directory"):
            install.checked_prefix(str(self.root / "to-usr"))
        self.assertEqual(install.checked_prefix(str(self.root / "fine")), self.root / "fine")
        # TMPDIR does not widen where a prefix may be (review).
        with patch.dict(os.environ, {"TMPDIR": "/usr/local"}), patch.object(tempfile, "tempdir", None):
            with self.assertRaisesRegex(install.InstallError, "never installed into a system directory"):
                install.checked_prefix("/usr/local/witchy")
        # A link inside the prefix where a directory is written: refused, nothing written there.
        prefix = install.Prefix(self.root / "prefix")
        (self.root / "prefix" / "bin").symlink_to(away)
        with self.assertRaisesRegex(install.InstallError, "is a symbolic link or not a directory"):
            prefix.write("bin/witchyc", b"x", 0o755)
        self.assertEqual(list(away.iterdir()), [])
        (self.root / "prefix" / "bin").unlink()
        # A link where a file is written is replaced, never written through.
        (self.root / "prefix" / "bin").mkdir(mode=0o755)
        (away / "target").write_text("kept")
        (self.root / "prefix" / "bin" / "grimoire").symlink_to(away / "target")
        prefix.write("bin/grimoire", b"new", 0o755)
        self.assertEqual((away / "target").read_text(), "kept")
        self.assertEqual((self.root / "prefix" / "bin" / "grimoire").read_bytes(), b"new")
        self.assertFalse((self.root / "prefix" / "bin" / "grimoire").is_symlink())
        # A link the installer makes points inside the prefix.
        for target in ("/usr/bin/python3", "../../away/target"):
            with self.subTest(target=target), self.assertRaisesRegex(install.InstallError, "stays inside the prefix"):
                prefix.link("bin/coven", target)
        # A directory others may write is no place for the engine and its library.
        os.chmod(self.root / "prefix" / "bin", 0o777)
        with self.assertRaisesRegex(install.InstallError, "writable by others"):
            prefix.write("bin/witchyc", b"x", 0o755)
        # The prefix itself, swapped for a link after it was checked: refused.
        (self.root / "swapped").symlink_to(away)
        with self.assertRaisesRegex(install.InstallError, "is a symbolic link or not a directory"):
            install.Prefix(self.root / "swapped")

    @unittest.skipUnless(sys.platform == "darwin" and os.environ.get("WITCHY_WASMTIME_HOST"), "the engine's library is a dylib on macOS")
    def test_the_engine_is_not_installed_without_its_crypto_library_beside_it(self):
        host = Path(shutil.which(os.environ["WITCHY_WASMTIME_HOST"]) or os.environ["WITCHY_WASMTIME_HOST"]).resolve()
        library = install.crypto_library(host, host.parent)
        self.assertRegex(library.name, r"^libaws_lc_fips_\w+_crypto\.dylib$")
        prefix = install.Prefix(self.root / "prefix")
        prefix.write("bin/witchy-wasmtime-host", host.read_bytes(), 0o755)
        with self.assertRaisesRegex(install.InstallError, "missing; the engine would not start"):
            install.check_engine(self.root / "prefix")
        prefix.write(Path("bin") / library.name, library.read_bytes(), 0o644)
        install.check_engine(self.root / "prefix")

    @unittest.skipUnless(sys.platform == "darwin", "the engine's library is a dylib on macOS")
    def test_two_different_builds_of_the_crypto_library_stop_the_install(self):
        release = self.root / "release"
        name = "libaws_lc_fips_0_14_4_crypto.dylib"
        for hash_, data in (("a", b"one"), ("b", b"two")):
            (release / f"build/aws-lc-fips-sys-{hash_}/out/build/artifacts").mkdir(parents=True)
            (release / f"build/aws-lc-fips-sys-{hash_}/out/build/artifacts" / name).write_bytes(data)
        listed = subprocess.CompletedProcess([], 0, f"host:\n\t@rpath/{name} (compatibility version 0.0.0)\n", "")
        with patch.object(install.subprocess, "run", return_value=listed):
            with self.assertRaisesRegex(install.InstallError, "different copies"):
                install.crypto_library(release / "host", release)
            (release / f"build/aws-lc-fips-sys-b/out/build/artifacts" / name).write_bytes(b"one")
            self.assertEqual(install.crypto_library(release / "host", release).read_bytes(), b"one")
            # MBX's layout: one directory per package, then one per build.
            (release / "build/aws-lc-fips-sys/c/out/build/artifacts").mkdir(parents=True)
            (release / "build/aws-lc-fips-sys/c/out/build/artifacts" / name).write_bytes(b"two")
            with self.assertRaisesRegex(install.InstallError, "different copies"):
                install.crypto_library(release / "host", release)
            (release / "build/aws-lc-fips-sys/c/out/build/artifacts" / name).write_bytes(b"one")
            self.assertEqual(install.crypto_library(release / "host", release).read_bytes(), b"one")


if __name__ == "__main__":
    unittest.main()
