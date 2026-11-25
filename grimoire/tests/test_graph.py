"""Graph decisions over package records. The registry integration tests cover signed fetch and build."""
import hashlib
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from registry_run import load_cli


class Graph(unittest.TestCase):
    def test_dependencies_name_exact_versions(self):
        cli = load_cli()
        with tempfile.TemporaryDirectory() as temp:
            manifest = Path(temp) / "witchy.toml"
            manifest.write_text('[registry]\npath = "."\n[dependencies]\nlib = "^0.1"\n')
            with self.assertRaisesRegex(cli.ProjectError, "exact MAJOR.MINOR.PATCH"):
                cli.read_dependencies(manifest)

    def test_conflicting_versions_and_cycles_report_the_complete_chains(self):
        cli = load_cli()
        with tempfile.TemporaryDirectory() as work, patch.dict(os.environ, XDG_STATE_HOME=work):
            registry = cli.Registry(str(Path(work) / "registry"), signed=False)
            store = cli.TrustStore(registry)
            self.addCleanup(store.close)
            rows = [
                ({"config-reader": "0.2.0", "digits": "2.0.0"}, "",
                 "digits is required at two versions: witchy.toml -> digits 2.0.0; witchy.toml -> config-reader 0.2.0 -> digits 1.0.0"),
                ({"config-reader": "0.2.0"}, "requires config-reader 0.2.0\n",
                 "digits 1.0.0 requires config-reader, which requires it: witchy.toml -> config-reader 0.2.0 -> digits 1.0.0 -> config-reader 0.2.0"),
            ]
            for roots, digit_requires, expected in rows:
                with self.subTest(roots=roots), tempfile.TemporaryDirectory(dir=work) as staging:
                    def supply_record(registry, name, version, into, *pins):
                        requires = "requires digits 1.0.0\n" if name == "config-reader" else digit_requires
                        into.mkdir(parents=True)
                        record = f"witchy-package 1\nname {name}\nversion {version}\n{requires}\n".encode()
                        (into / "package").write_bytes(record)
                        (into / "source.wy").write_bytes(b"")
                        result = cli.Fetched()
                        result.ok = True
                        result.digest = "sha384:" + hashlib.sha384(record).hexdigest()
                        return result

                    with patch.object(cli, "coven_fetch", supply_record), self.assertRaises(cli.ProjectError) as refused:
                        cli.resolve_with(registry, roots, {}, staging, False, store, None)
                    self.assertEqual(str(refused.exception), expected)

    def test_staged_package_list_preserves_only_each_lock_records_own_edges(self):
        graph = {"app-lib": ("1.0.0", "digest", {"leaf": "1.0.0"}, Path("one"), "unsigned", 0, "none"),
                 "leaf": ("1.0.0", "digest", {}, Path("two"), "unsigned", 0, "none")}
        cli = load_cli()
        with self.assertRaisesRegex(cli.ProjectError, "app-lib, leaf: no verified release was staged"):
            cli.package_listing(graph)
        class Verified(dict):
            staged = {name: type("Staged", (), {"entry": row[3] / "source.wy"}) for name, row in graph.items()}
        self.assertEqual(cli.package_listing(Verified(graph)),
                         ["app-lib 1.0.0: one/source.wy ; requires leaf", "leaf 1.0.0: two/source.wy ; requires"])

if __name__ == "__main__":
    unittest.main()
