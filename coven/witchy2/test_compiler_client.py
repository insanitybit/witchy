"""The compiler client's reviewed build, against a stand-in compiler session."""
import contextlib
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import witchy_compiler


class ReviewedBuild(unittest.TestCase):
    def test_paths_resolve_against_the_callers_directory_when_named(self):
        """The entry is the one reviewed; the module lands where the caller named it, even if the
        process changes directory while the compiler builds."""
        with tempfile.TemporaryDirectory() as temp:
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
            checked = [wire for wire in requests if wire[0] == "build-checked"][0]
            self.assertEqual(checked[2], str(root / "review/main.wy"))
            self.assertEqual((root / "build/module.wasm").read_bytes(), b"\0asm")
            self.assertFalse((root / "elsewhere/module.wasm").exists())


if __name__ == "__main__":
    unittest.main()
