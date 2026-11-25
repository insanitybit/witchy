"""Edit loop for lock migration diagnostics. SnapshotRun also exercises real signed registries."""
from importlib.machinery import SourceFileLoader
from importlib.util import module_from_spec, spec_from_loader
from pathlib import Path
import tempfile
import unittest


class LockFormat(unittest.TestCase):
    def test_package_pins_use_the_current_digest(self):
        loader = SourceFileLoader("grimoire_cli", str(Path(__file__).resolve().parents[1] / "grimoire"))
        cli = module_from_spec(spec_from_loader(loader.name, loader))
        loader.exec_module(cli)
        with tempfile.TemporaryDirectory() as temp:
            lock = Path(temp) / "witchy.lock"
            fields = '[[package]]\nname = "lib"\nversion = "1.0.0"\nrequires = []\nsigner = "unsigned"\nrecovery = "none"\nindex = 0\n'
            current = "sha384:" + "0" * 96
            lock.write_text(fields + f'digest = "{current}"\n')
            self.assertEqual(cli.parse_lock(lock)["lib"][1], current)
            lock.write_text(fields + 'digest = "sha256:' + "0" * 64 + '"\n')
            with self.assertRaisesRegex(cli.ProjectError, "is pinned by a sha256 digest") as refused:
                cli.parse_lock(lock)
            self.assertIn("Remove it and lock again", str(refused.exception))


if __name__ == "__main__":
    unittest.main()
