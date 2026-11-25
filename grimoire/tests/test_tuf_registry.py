"""Check Grimoire's TUF resolver and lock through the Coven command boundary."""
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import tomllib
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from registry_run import load_cli
from test_release_format import codec


def digest(wire):
    return "sha384:" + hashlib.sha384(wire).hexdigest()


class TufRegistry(unittest.TestCase):
    def setUp(self):
        self.cli = load_cli()
        self.project = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        self.manifest = self.project / "witchy.toml"
        self.lock = self.project / "witchy.lock"
        self.root_pin = digest(b"root")
        self.trust_pin = digest(b"release trust")
        self.ca_pin = digest(b"CA bundle")
        self.config = f'''[registry]
protocol = "tuf"
url = "https://packages.example.test"
id = "test"
root = "root.json"
root-pin = "{self.root_pin}"
trust = "trust.json"
trust-pin = "{self.trust_pin}"
ca-file = "ca.pem"
ca-pin = "{self.ca_pin}"

[dependencies]
app = "1.0.0"
'''
        self.manifest.write_text(self.config)
        self.sources = {
            name: codec.pack(name, "1.0.0", requires, "lib.wy", {"lib.wy": b"fn answer() -> U64:\n    return 42\n"})
            for name, requires in (("app", {"lib": "1.0.0"}), ("lib", {}))
        }
        self.version = 3
        self.snapshot = digest(b"snapshot 3")
        self.publisher = digest(b"publisher")
        self.overrides = {}
        self.calls = []
        self.timeouts = []
        self.enterContext(patch.object(self.cli, "tool", return_value="/tool/coven"))
        self.enterContext(patch.object(self.cli, "run_coven", side_effect=self.fetch))

    def fetch(self, command, *, timeout):
        self.assertEqual(command[:3], ["/tool/coven", "tuf", "fetch"])
        name, version = command[3:5]
        self.assertEqual(version, "1.0.0")
        arguments = dict(zip(command[5::2], command[6::2], strict=True))
        self.calls.append((name, arguments))
        self.timeouts.append(timeout)
        payload = self.sources[name]
        facts = {"digest": digest(payload), "publisher": self.publisher,
                 "snapshot": self.snapshot, "snapshot_version": self.version,
                 "target": digest(name.encode()), "ca_pin": self.ca_pin} | self.overrides.get(name, {})
        Path(arguments["--out"]).write_bytes(payload)
        return subprocess.CompletedProcess(command, 0, json.dumps(facts), "")

    def read_lock(self, *, catalog_snapshot=None, catalog_ca_pin=None):
        with tempfile.TemporaryDirectory() as temp:
            registry, dependencies = self.cli.read_dependencies(self.manifest)
            graph = self.cli.read_lock(self.manifest, dependencies, registry, temp, catalog_snapshot=catalog_snapshot,
                                       catalog_ca_pin=catalog_ca_pin)
            return {name: staged.entry.read_bytes() for name, staged in graph.staged.items()}

    def test_lock_and_build_use_exact_pins_and_a_single_snapshot(self):
        self.cli.lock_project(self.project)
        saved = tomllib.loads(self.lock.read_text())
        self.assertEqual(saved["tuf"], {
            "url": "https://packages.example.test", "id": "test", "root-pin": self.root_pin,
            "trust-pin": self.trust_pin, "snapshot": self.snapshot, "snapshot-version": 3, "ca-pin": self.ca_pin,
        })
        self.assertEqual(saved["package"][0], {
            "name": "app", "version": "1.0.0", "requires": ["lib 1.0.0"],
            "digest": digest(self.sources["app"]), "publisher": self.publisher, "target": digest(b"app"),
        })
        self.assertNotIn("--expect-snapshot", self.calls[0][1])
        self.assertEqual(len(self.timeouts), 2)
        self.assertTrue(0 < self.timeouts[1] <= self.timeouts[0] <= 60)
        self.assertEqual(self.calls[1][1]["--expect-snapshot"], self.snapshot)
        self.assertEqual(self.calls[0][1]["--ca-file"], self.project / "ca.pem")

        # A newer verified generation is usable if every locked release is unchanged.
        self.version = 4
        self.snapshot = digest(b"snapshot 4")
        self.calls.clear()
        self.assertEqual(set(self.read_lock()), {"app", "lib"})
        for name, arguments in self.calls:
            self.assertEqual(arguments["--root-pin"], self.root_pin)
            self.assertEqual(arguments["--trust-pin"], self.trust_pin)
            self.assertEqual(arguments["--ca-pin"], self.ca_pin)
            self.assertEqual(arguments["--expect"], digest(self.sources[name]))
            self.assertEqual(arguments["--expect-target"], digest(name.encode()))
        self.assertEqual(self.calls[1][1]["--expect-snapshot"], self.snapshot)

    def test_build_uses_the_lock_captured_for_the_toolchain_check(self):
        self.manifest.write_text('[run]\nentry = "main.wy"\n\n' + self.config)
        entry = self.project / "main.wy"
        entry.write_text('import "@app"\n')
        pin = digest(b"fn answer() -> U64:\n    return 42\n")
        entry.with_suffix(".policy").write_text(f"@app: nothing ; digest {pin}\n@lib: nothing ; digest {pin}\n")
        self.cli.lock_project(self.project)
        tools = {}
        for role, (variable, _) in self.cli.TOOLS.items():
            tool = self.project / role
            tool.write_bytes(role.encode())
            tools[variable] = tool
        pins = "\n".join(f'{role} = "{digest(tools[variable].read_bytes())}"'
                         for role, (variable, _) in self.cli.TOOLS.items())
        original = self.lock.read_bytes() + f"\n[toolchain]\n{pins}\n".encode()
        self.lock.write_bytes(original)
        replacement = original.replace(b"snapshot-version = 3", b"snapshot-version = 4")
        check_toolchain = self.cli.check_toolchain
        output = self.project / "program.wasm"

        def check_and_replace(manifest, **options):
            check_toolchain(manifest, **options)
            self.lock.write_bytes(replacement)

        @contextmanager
        def reviewed_build(*args, **kwargs):
            yield subprocess.CompletedProcess([], 0, "", ""), lambda path: path.write_bytes(b"built")

        compiler = SimpleNamespace(reviewed_build=reviewed_build)
        with (patch.object(self.cli, "check_toolchain", side_effect=check_and_replace),
              patch.object(self.cli, "compiler_client", return_value=compiler),
              patch.object(self.cli, "tool", side_effect=lambda name, _: tools.get(name, "/tool/coven"))):
            self.cli.build_project(self.project, output)
        self.assertEqual(output.read_bytes(), b"built")
        self.assertEqual(self.lock.read_bytes(), replacement)

    def test_build_with_no_captured_lock_refuses_a_lock_that_appears_later(self):
        self.manifest.write_text('[run]\nentry = "main.wy"\n\n' + self.config)
        entry = self.project / "main.wy"
        entry.write_text('import "@app"\n')
        entry.with_suffix(".policy").write_text("# policy\n")
        self.cli.lock_project(self.project)
        wire = self.lock.read_bytes()
        self.lock.unlink()
        self.calls.clear()
        check_toolchain = self.cli.check_toolchain

        def check_and_install(manifest, **options):
            check_toolchain(manifest, **options)
            self.lock.write_bytes(wire)

        with patch.object(self.cli, "check_toolchain", side_effect=check_and_install):
            with self.assertRaisesRegex(self.cli.ProjectError, "no witchy.lock beside witchy.toml"):
                self.cli.build_project(self.project, self.project / "program.wasm")
        self.assertEqual(self.calls, [])
        self.assertEqual(self.lock.read_bytes(), wire)

    def test_lock_does_not_overwrite_a_concurrent_replacement(self):
        self.cli.lock_project(self.project)
        replacement = self.lock.read_bytes() + b"# concurrently replaced lock\n"

        def concurrent_lock(command, *, timeout):
            result = self.fetch(command, timeout=timeout)
            if command[3] == "lib":
                self.lock.write_bytes(replacement)
            return result

        with patch.object(self.cli, "run_coven", side_effect=concurrent_lock):
            with self.assertRaisesRegex(self.cli.ProjectError, "changed during dependency locking"):
                self.cli.lock_project(self.project)
        self.assertEqual(self.lock.read_bytes(), replacement)

    def test_https_resolution_keeps_one_verified_ca_bundle(self):
        self.overrides["app"] = {"ca_pin": digest(b"another CA bundle")}
        with self.assertRaisesRegex(self.cli.ProjectError, "HTTPS CA bundle differs from the manifest pin"):
            self.cli.lock_project(self.project)
        self.assertFalse(self.lock.exists())
        self.assertEqual([name for name, _ in self.calls], ["app"])

        self.calls.clear()
        self.overrides["lib"] = self.overrides.pop("app")
        self.manifest.write_text(self.config.replace('ca-file = "ca.pem"\n', "").replace(f'ca-pin = "{self.ca_pin}"\n', ""))
        with self.assertRaisesRegex(self.cli.ProjectError, "HTTPS CA bundle changed during dependency resolution"):
            self.cli.lock_project(self.project)
        self.assertFalse(self.lock.exists())
        self.assertEqual([name for name, _ in self.calls], ["app", "lib"])

        self.assertNotIn("--ca-pin", self.calls[0][1])
        self.assertEqual(self.calls[1][1]["--ca-pin"], self.ca_pin)

    def test_catalog_ca_bundle_applies_even_to_the_first_fetch(self):
        self.manifest.write_text(self.config.replace('ca-file = "ca.pem"\n', "")
                                 .replace(f'ca-pin = "{self.ca_pin}"\n', ""))
        self.cli.lock_project(self.project)
        self.assertNotIn("ca-pin", tomllib.loads(self.lock.read_text())["tuf"])
        self.calls.clear()
        expected = (self.snapshot, self.version)
        self.assertEqual(set(self.read_lock(catalog_snapshot=expected, catalog_ca_pin=self.ca_pin)), {"app", "lib"})
        self.assertEqual([flags["--ca-pin"] for _, flags in self.calls], [self.ca_pin, self.ca_pin])
        self.calls.clear()
        self.overrides["app"] = {"ca_pin": digest(b"another CA bundle")}
        with self.assertRaisesRegex(self.cli.ProjectError, "HTTPS CA bundle changed during dependency resolution"):
            self.read_lock(catalog_snapshot=expected, catalog_ca_pin=self.ca_pin)
        self.assertEqual([name for name, _ in self.calls], ["app"])

    def test_manifest_pins_the_default_https_ca_bundle_from_the_first_fetch(self):
        self.manifest.write_text(self.config.replace('ca-file = "ca.pem"\n', ""))
        self.overrides["app"] = {"ca_pin": digest(b"another CA bundle")}
        with self.assertRaisesRegex(self.cli.ProjectError, "HTTPS CA bundle differs from the manifest pin"):
            self.cli.lock_project(self.project)
        self.assertFalse(self.lock.exists())
        self.assertEqual([name for name, _ in self.calls], ["app"])
        self.calls.clear()
        self.overrides.clear()
        self.cli.lock_project(self.project)
        self.assertEqual([flags["--ca-pin"] for _, flags in self.calls], [self.ca_pin, self.ca_pin])
        self.assertTrue(all("--ca-file" not in flags for _, flags in self.calls))

    def test_a_pinned_https_ca_changes_only_through_lock_without_rollback(self):
        self.cli.lock_project(self.project)
        original = self.lock.read_bytes()
        new_pin = digest(b"renewed CA bundle")
        self.calls.clear()
        self.manifest.write_text(self.config.replace(self.ca_pin, new_pin))
        with self.assertRaisesRegex(self.cli.ProjectError, "pins a different HTTPS CA bundle"):
            self.read_lock()
        with self.assertRaisesRegex(self.cli.ProjectError, "pins a different HTTPS CA bundle"):
            self.cli.lock_project(self.project, recovery_pin=self.snapshot)
        self.assertFalse(self.calls)

        self.overrides = {name: {"ca_pin": new_pin} for name in self.sources}
        original_snapshot = self.snapshot
        self.snapshot, self.version = digest(b"snapshot 2"), 2
        with self.assertRaisesRegex(self.cli.ProjectError, "rolls back"):
            self.cli.lock_project(self.project)
        self.assertEqual(self.lock.read_bytes(), original)

        self.snapshot, self.version = original_snapshot, 3
        self.cli.lock_project(self.project)
        self.assertEqual(tomllib.loads(self.lock.read_text()), tomllib.loads(original.decode().replace(self.ca_pin, new_pin)))
        self.assertEqual(set(self.read_lock()), {"app", "lib"})
        self.calls.clear()
        self.manifest.write_text(self.config.replace(f'ca-pin = "{self.ca_pin}"\n', ""))
        for run in (self.read_lock, lambda: self.cli.lock_project(self.project)):
            with self.assertRaisesRegex(self.cli.ProjectError, "Restore ca-pin in the manifest"):
                run()
        self.assertFalse(self.calls)

    def test_lock_upgrades_an_older_lock_without_a_ca_pin(self):
        self.cli.lock_project(self.project)
        pinned = self.lock.read_bytes()
        self.lock.write_bytes(pinned.replace(f'ca-pin = "{self.ca_pin}"\n'.encode(), b""))
        self.assertEqual(set(self.read_lock()), {"app", "lib"})
        self.cli.lock_project(self.project)
        self.assertEqual(self.lock.read_bytes(), pinned)

    def test_ca_pin_requires_an_https_origin_and_canonical_digest(self):
        for config in (self.config.replace('ca-file = "ca.pem"\n', "").replace(
                           "https://packages.example.test", "http://127.0.0.1:12345"),
                       self.config.replace(self.ca_pin, self.ca_pin.upper())):
            with self.subTest(config=config):
                self.manifest.write_text(config)
                with self.assertRaisesRegex(self.cli.ProjectError, "ca-pin requires an HTTPS origin"):
                    self.cli.lock_project(self.project)
                self.assertFalse(self.lock.exists())
        self.assertEqual(self.calls, [])

    def test_tuf_lock_rejects_a_cycle_between_direct_dependencies(self):
        self.manifest.write_text(self.config + 'lib = "1.0.0"\n')
        self.sources["lib"] = codec.pack("lib", "1.0.0", {"app": "1.0.0"}, "lib.wy",
                                         {"lib.wy": b"fn answer() -> U64:\n    return 42\n"})
        cycle = "witchy.toml -> app 1.0.0 -> lib 1.0.0 -> app 1.0.0"
        with self.assertRaisesRegex(self.cli.ProjectError, cycle):
            self.cli.lock_project(self.project)
        self.assertFalse(self.lock.exists())
        self.assertEqual([name for name, _ in self.calls], ["app", "lib"])

        # Model a lock written by the discovery walk before cycle checking.
        with patch.object(self.cli, "check_dependency_cycles"):
            self.cli.lock_project(self.project)
        previous = self.lock.read_bytes()
        with self.assertRaisesRegex(self.cli.ProjectError, cycle):
            self.read_lock()
        self.assertEqual(self.lock.read_bytes(), previous)

    def test_duplicate_fields_are_refused_before_a_lock_is_published(self):
        def duplicate(command, *, timeout):
            result = self.fetch(command, timeout=timeout)
            result.stdout = '{"snapshot_version": 2, ' + result.stdout[1:]
            return result

        with patch.object(self.cli, "run_coven", side_effect=duplicate):
            with self.assertRaisesRegex(self.cli.ProjectError, "invalid Coven TUF fetch result"):
                self.cli.lock_project(self.project)
        self.assertFalse(self.lock.exists())
        self.assertEqual([name for name, _ in self.calls], ["app"])

    def test_catalog_receipt_refuses_duplicate_decoded_member_names(self):
        catalog = self.cli.web_builder().coven_catalog_web
        response = r'{"releases":[{"name":"lib","\u006eame":"app"}]}'
        with patch.object(self.cli, "run_coven", return_value=subprocess.CompletedProcess([], 0, response, "")):
            with self.assertRaisesRegex(ValueError, "duplicate field in Coven receipt"):
                catalog.read_catalog(self.cli, [], 60)

    def test_catalog_renders_only_the_exact_document_for_the_verified_release(self):
        catalog = self.cli.web_builder().coven_catalog_web
        target = digest(b"target")
        release = {"name": "lib", "version": "1.0.0", "target": target, "length": 6}
        verified = {"registry": "test", "snapshot": self.snapshot, "snapshot_version": self.version,
                    "timestamp": digest(b"timestamp"), "releases": [release]}
        catalog.validate_catalog(verified, "test", tuf=True)
        table = b"GDL1\nT7\nPackage7\nVersionL3\nlib5\n1.0.020\n/packages/lib/1.0.0/EZ"
        detail = (b"GDL1\nH3\nlibD7\nVersion5\n1.0.0D20\nSealed target digest103\n"
                  + target.encode("ascii") + b"A13\nBack to Coven1\n/Z")

        def pages(table_bytes, detail_bytes):
            replies = {"C": table_bytes, "D": detail_bytes}
            return list(catalog.render_pages(lambda mode, fields, limit: replies[mode], verified))

        index, package = pages(table, detail)
        self.assertEqual((index[0], package[0]), ("index.html", "packages/lib/1.0.0/index.html"))
        self.assertIn('<a href="/packages/lib/1.0.0/">lib</a>', index[2])
        self.assertIn(target, package[2])
        for changed in (table.replace(b"L3\nlib", b"L3\napp"), table[:-1], table + b"Z"):
            with self.subTest(table=changed), self.assertRaisesRegex(ValueError, "expected catalog table"):
                pages(changed, detail)
        with self.assertRaisesRegex(ValueError, "expected package detail"):
            pages(table, detail.replace(target.encode("ascii"), digest(b"other").encode("ascii")))

    def test_https_catalog_requires_matching_effective_ca_and_keeps_it_out_of_the_public_catalog(self):
        catalog = self.cli.web_builder().coven_catalog_web
        document = {"registry": "test", "snapshot": self.snapshot, "snapshot_version": self.version,
                    "timestamp": digest(b"timestamp"), "releases": []}
        registry, _ = self.cli.read_dependencies(self.manifest)
        for pin in (None, self.ca_pin, digest(b"another CA bundle")):
            receipt = document if pin is None else document | {"ca_pin": pin}
            result = subprocess.CompletedProcess([], 0, json.dumps(receipt), "")
            with self.subTest(pin=pin), patch.object(self.cli, "run_coven", return_value=result) as run:
                if pin == self.ca_pin:
                    self.assertEqual(catalog.catalog(self.cli, registry, None, ca_pin=pin), (document, pin))
                    self.assertEqual(run.call_args.args[0][-2:], ["--ca-pin", pin])
                else:
                    with self.assertRaisesRegex(ValueError, "verified catalog HTTPS CA bundle"):
                        catalog.catalog(self.cli, registry, None)

    def test_default_resolution_deadline_refuses_an_expired_graph(self):
        with patch.object(self.cli.time, "monotonic", side_effect=[10, 10, 69.5, 70]):
            with self.assertRaisesRegex(self.cli.ProjectError, r"resolution-deadline-ms \(60000 ms\)"):
                self.cli.lock_project(self.project)
        self.assertEqual(self.timeouts, [60.0, 0.5])
        self.assertFalse(self.lock.exists())

    def test_resolution_shares_the_deadline_across_packages(self):
        self.cli.lock_project(self.project)
        locked = self.lock.read_bytes()
        self.manifest.write_text(self.config.replace("[dependencies]", "resolution-deadline-ms = 1000\n\n[dependencies]"))
        self.timeouts.clear()

        def delayed_fetch(command, *, timeout):
            if command[3] == "lib":
                self.timeouts.append(timeout)
                raise subprocess.TimeoutExpired(command, timeout)
            return self.fetch(command, timeout=timeout)

        with patch.object(self.cli.time, "monotonic", side_effect=[10, 10, 10.75]), \
                patch.object(self.cli, "run_coven", side_effect=delayed_fetch):
            with self.assertRaisesRegex(self.cli.ProjectError, "resolution-deadline-ms"):
                self.cli.lock_project(self.project)
        self.assertEqual(self.timeouts, [1.0, 0.25])
        self.assertEqual(self.lock.read_bytes(), locked)

    def test_resolution_refuses_when_locally_staged_graph_runs_out_of_time(self):
        self.manifest.write_text(self.config.replace("[dependencies]", "resolution-deadline-ms = 1000\n\n[dependencies]"))
        with patch.object(self.cli.time, "monotonic", side_effect=[10, 10, 10.5, 11]):
            with self.assertRaisesRegex(self.cli.ProjectError, "resolution-deadline-ms"):
                self.cli.lock_project(self.project)
        self.assertEqual(self.timeouts, [1.0, 0.5])
        self.assertFalse(self.lock.exists())

    def test_resolution_deadline_requires_an_integer_in_range(self):
        for value in ("true", "1.5", "0", "-1", "2147483648"):
            with self.subTest(value=value):
                manifest = self.config.replace("[dependencies]", f"resolution-deadline-ms = {value}\n\n[dependencies]")
                self.manifest.write_text(manifest)
                with self.assertRaisesRegex(self.cli.ProjectError, "resolution-deadline-ms"):
                    self.cli.read_dependencies(self.manifest)

    def test_catalog_snapshot_applies_even_to_the_first_fetch(self):
        self.cli.lock_project(self.project)
        expected = (self.snapshot, self.version)
        self.calls.clear()
        self.assertEqual(set(self.read_lock(catalog_snapshot=expected)), {"app", "lib"})
        self.assertEqual([flags["--expect-snapshot"] for _, flags in self.calls], [expected[0], expected[0]])
        self.overrides = {"app": {"snapshot_version": self.version + 1}}
        self.calls.clear()
        with self.assertRaisesRegex(self.cli.ProjectError, "differs from the required TUF snapshot"):
            self.read_lock(catalog_snapshot=expected)
        self.assertEqual([name for name, _ in self.calls], ["app"])

    def test_stage_keeps_the_policy_checked_against_the_registry_graph(self):
        entry = self.project / "main.wy"
        entry.write_text('import "@app"\n')
        policy = entry.with_suffix(".policy")
        pin = digest(b"fn answer() -> U64:\n    return 42\n")
        checked = f"@app: nothing ; digest {pin}\n@lib: nothing ; digest {pin}\n".encode()
        policy.write_bytes(checked)
        self.cli.lock_project(self.project)
        check_packages = self.cli.unpinned_files

        def replace_after_check(text, releases):
            result = check_packages(text, releases)
            policy.write_text("# replacement after graph check\n")
            return result

        with tempfile.TemporaryDirectory() as temp, patch.object(self.cli, "unpinned_files", replace_after_check):
            staged, _ = self.cli.stage_registry_packages(self.manifest, entry, temp)
            self.assertEqual(staged.with_suffix(".policy").read_bytes(), checked)
        self.assertNotEqual(policy.read_bytes(), checked)

    def test_oversized_policy_is_refused_before_fetching(self):
        entry = self.project / "main.wy"
        entry.write_text('import "@app"\n')
        with entry.with_suffix(".policy").open("wb") as policy:
            policy.truncate(self.cli.CONTROL_FILE_BYTES + 1)
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaisesRegex(self.cli.ProjectError, "control file exceeds"):
                self.cli.stage_registry_packages(self.manifest, entry, temp, require_policy=False)
        self.assertEqual(self.calls, [])

    def test_accept_preserves_a_policy_edited_while_reviewing(self):
        self.manifest.write_text(self.config + '\n[run]\nentry = "main.wy"\n')
        entry = self.project / "main.wy"
        entry.write_text('import "@app"\nfn main():\n    return\n')
        policy = entry.with_suffix(".policy")
        pin = digest(b"fn answer() -> U64:\n    return 42\n")
        original = f"@app: nothing ; digest {pin}\n@lib: nothing ; digest {pin}\nmain.wy: nothing\n"
        replacement = original.replace("main.wy: nothing", "main.wy: Allocator")
        policy.write_text(original)
        self.cli.lock_project(self.project)

        def run(command, *, cwd, **options):
            operation, *args = command[2:]
            if operation == "--repin":
                self.assertEqual((cwd / args[0]).read_text(), original)
                policy.write_text(replacement)
            return subprocess.CompletedProcess(command, 0, "", "")

        with (patch.object(self.cli, "run_project", return_value=None),
              patch.object(self.cli, "compiler_client", return_value=SimpleNamespace(run=run))):
            with self.assertRaisesRegex(self.cli.ProjectError, "changed during policy review"):
                self.cli.review_project(self.project, accept=True)
        self.assertEqual(policy.read_text(), replacement)

    def test_draft_never_replaces_a_policy_written_during_review(self):
        self.manifest.write_text(self.config + '\n[run]\nentry = "main.wy"\n')
        entry = self.project / "main.wy"
        entry.write_text('import "@app"\nfn main():\n    return\n')
        policy = entry.with_suffix(".policy")
        replacement = "# written while the compiler was drafting\nmain.wy: Allocator\n"
        self.cli.lock_project(self.project)

        def run(command, *, cwd, **options):
            if command[2] == "--draft-policy":
                policy.write_text(replacement)
            return subprocess.CompletedProcess(command, 0, "main.wy: nothing\n", "")

        with (patch.object(self.cli, "run_project", return_value=None),
              patch.object(self.cli, "compiler_client", return_value=SimpleNamespace(run=run))):
            with self.assertRaisesRegex(self.cli.ProjectError, "changed during policy review"):
                self.cli.review_project(self.project, accept=False, write_policy=True)
        self.assertEqual(policy.read_text(), replacement)

    def test_record_keeps_the_build_snapshot_and_original_lock(self):
        entry = self.project / "main.wy"
        entry.write_text('import "@app"\n')
        self.cli.lock_project(self.project)
        original = self.lock.read_bytes()
        self.version = 4
        self.snapshot = digest(b"snapshot 4")
        with tempfile.TemporaryDirectory() as temp:
            staged, graph = self.cli.stage_registry_packages(
                self.manifest, entry, temp, require_policy=False)
            # Another invocation can replace the project lock while this build is running.
            self.lock.write_bytes(b"replacement")
            facts = self.cli.capture_build_record(self.manifest, staged, None, [], self.cli.file_digest(staged), staged, staged, graph=graph)
        self.assertEqual(facts["lock"], digest(original))
        self.assertEqual(facts["tuf"], {"snapshot": self.snapshot, "snapshot-version": 4, "ca-pin": self.ca_pin})
        for name, payload in self.sources.items():
            self.assertEqual(facts["packages"][name]["digest"], digest(payload))

    def test_refusals_leave_the_lock_intact(self):
        self.cli.lock_project(self.project)
        locked = self.lock.read_bytes()
        cases = (
            ("app", {"snapshot_version": 2}, "rolls back"),
            ("app", {"snapshot": digest(b"different at version 3")}, "conflicts"),
            ("lib", {"snapshot": digest(b"another snapshot"), "snapshot_version": 4}, "changed during resolution"),
            ("lib", {"publisher": digest(b"other publisher")}, "publisher differs"),
            ("lib", {"target": digest(b"other target")}, "release differs"),
        )
        for name, override, reason in cases:
            with self.subTest(reason=reason):
                self.overrides = {name: override}
                with self.assertRaisesRegex(self.cli.ProjectError, reason):
                    self.cli.lock_project(self.project)
                self.assertEqual(self.lock.read_bytes(), locked)

        self.overrides = {}
        self.manifest.write_text(self.config.replace(self.trust_pin, digest(b"other trust")))
        self.calls.clear()
        with self.assertRaisesRegex(self.cli.ProjectError, "different registry or trust policy"):
            self.read_lock()
        self.assertFalse(self.calls)

    def test_recover_a_snapshot_with_an_independent_pin_and_the_locked_graph(self):
        self.cli.lock_project(self.project)
        original = self.lock.read_bytes()
        before = tomllib.loads(original.decode())
        self.version = 1
        self.snapshot = digest(b"recovered snapshot 1")
        with self.assertRaisesRegex(self.cli.ProjectError, "rolls back"):
            self.cli.lock_project(self.project)

        for override, reason in (({"snapshot": digest(b"different snapshot")}, "differs from the recovery pin"),
                                 ({"target": digest(b"changed target")}, "TUF release differs")):
            self.overrides = {"lib": override}
            with self.assertRaisesRegex(self.cli.ProjectError, reason):
                self.cli.lock_project(self.project, recovery_pin=self.snapshot)
            self.assertEqual(self.lock.read_bytes(), original)
        self.overrides.clear()
        self.manifest.write_text(self.config.replace('app = "1.0.0"', 'lib = "1.0.0"'))
        with self.assertRaisesRegex(self.cli.ProjectError, "pins packages the dependencies do not require"):
            self.cli.lock_project(self.project, recovery_pin=self.snapshot)
        self.assertEqual(self.lock.read_bytes(), original)
        self.manifest.write_text(self.config)

        self.calls.clear()
        self.cli.lock_project(self.project, recovery_pin=self.snapshot)
        recovered = tomllib.loads(self.lock.read_text())
        before["tuf"].update({"snapshot": self.snapshot, "snapshot-version": 1})
        self.assertEqual(recovered, before)
        self.assertEqual([name for name, _ in self.calls], ["app", "lib"])
        for name, arguments in self.calls:
            self.assertEqual(arguments["--expect-snapshot"], self.snapshot)
            self.assertEqual(arguments["--expect"], digest(self.sources[name]))
            self.assertEqual(arguments["--expect-target"], digest(name.encode()))
        self.assertEqual(set(self.read_lock()), {"app", "lib"})

    def test_recovery_requires_a_matching_existing_lock(self):
        with self.assertRaisesRegex(self.cli.ProjectError, "existing witchy.lock"):
            self.cli.lock_project(self.project, recovery_pin=self.snapshot)
        self.assertFalse(self.calls)
        self.cli.lock_project(self.project)
        replacement = b"# concurrently replaced lock\n"

        def concurrent_lock(command, *, timeout):
            result = self.fetch(command, timeout=timeout)
            if command[3] == "lib":
                self.lock.write_bytes(replacement)
            return result

        with patch.object(self.cli, "run_coven", side_effect=concurrent_lock):
            with self.assertRaisesRegex(self.cli.ProjectError, "changed during TUF recovery"):
                self.cli.lock_project(self.project, recovery_pin=self.snapshot)
        self.assertEqual(self.lock.read_bytes(), replacement)

    def test_payload_is_checked_independently_of_the_fetch_receipt(self):
        self.overrides = {"app": {"digest": digest(b"another payload")}}
        with self.assertRaisesRegex(self.cli.ProjectError, "digest differs from the verified digest"):
            self.cli.lock_project(self.project)
        self.assertFalse(self.lock.exists())


if __name__ == "__main__":
    unittest.main()
