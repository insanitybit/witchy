"""Each layer of launch-environment scrubbing on its own. In a real launch each hides the other's
removal (a variable taken out of the process never reaches the toolchain's environment, and the
toolchain's environment never carries one left in the process), so each is checked directly."""
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

from registry_run import load_cli

ROOT = Path(__file__).resolve().parents[1]
CANARY = "canary-5d1e"


class ToolchainEnvironment(unittest.TestCase):
    def test_a_step_gets_only_the_toolchains_variables(self):
        """Layer 1: whatever this process still holds, a step started by Grimoire is given only
        the toolchain's variables (`toolchain_environment`)."""
        cli = load_cli()
        planted = {"REPO_TOKEN": CANARY, "WITCHY_SECRET_X": CANARY, "COVEN_TOKEN": CANARY,
                   "PATH": "/usr/bin:/bin", "WITCHYC": "/opt/witchyc", "SSL_CERT_FILE": "/test/roots.pem"}
        with patch.dict(os.environ, planted, clear=True):
            self.assertEqual(cli.toolchain_environment(), {"PATH": "/usr/bin:/bin", "WITCHYC": "/opt/witchyc"})

    def test_only_tuf_commands_receive_the_configured_ca_file(self):
        cli = load_cli()
        with patch.dict(os.environ, {"SSL_CERT_FILE": "/test/roots.pem", "REPO_TOKEN": CANARY}, clear=True):
            with patch.object(cli.subprocess, "run") as run:
                for args in (("tuf", "fetch"), ("tuf", "catalog"), ("key",)):
                    with self.subTest(args=args):
                        cli.run_coven(["coven", *args])
                        env = run.call_args.kwargs["env"]
                        self.assertNotIn("REPO_TOKEN", env)
                        self.assertEqual(env.get("SSL_CERT_FILE"), "/test/roots.pem" if args[0] == "tuf" else None)


# Run in a process of its own: taking the environment changes the process for good.
TAKE = r"""
import ctypes, importlib.machinery, importlib.util, json, os, subprocess, sys
loader = importlib.machinery.SourceFileLoader("grimoire_cli", sys.argv[1])
cli = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(cli)
taken = cli.take_launch_environment()
# PR_GET_DUMPABLE reports the actual kernel setting after the launch scrub.
dumpable = ctypes.CDLL(None).prctl(3, 0, 0, 0, 0) if sys.platform.startswith("linux") else None
child = subprocess.run([sys.executable, "-c", "import os, json; print(json.dumps(dict(os.environ)))"],
                       capture_output=True, text=True, check=True).stdout
print(json.dumps({"taken": {k: v.decode() for k, v in taken.items()}, "environ": dict(os.environ),
                  "getenv": os.getenv("REPO_TOKEN"), "child": json.loads(child), "dumpable": dumpable}))
"""


class LaunchEnvironment(unittest.TestCase):
    def test_the_launch_environment_is_taken_out_of_the_process(self):
        """Layer 2: every variable outside the toolchain's is taken out of Grimoire's own process
        (`take_launch_environment`), so nothing it starts without an explicit environment, and
        nothing reading its environment, finds one."""
        launch = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": os.environ.get("HOME", "/"),
                  "REPO_TOKEN": CANARY, "WITCHY_SECRET_X": CANARY}
        ran = subprocess.run([sys.executable, "-I", "-c", TAKE, str(ROOT / "grimoire")], env=launch,
                             capture_output=True, text=True, timeout=60)
        self.assertEqual(ran.returncode, 0, ran.stderr)
        seen = json.loads(ran.stdout)
        self.assertEqual({k: seen["taken"].get(k) for k in ("REPO_TOKEN", "WITCHY_SECRET_X")},
                         {"REPO_TOKEN": CANARY, "WITCHY_SECRET_X": CANARY})
        self.assertNotIn("PATH", seen["taken"])
        self.assertIsNone(seen["getenv"])
        if sys.platform.startswith("linux"):
            self.assertEqual(seen["dumpable"], 0)
        for where in ("environ", "child"):
            with self.subTest(where):
                self.assertFalse({"REPO_TOKEN", "WITCHY_SECRET_X"} & set(seen[where]), seen[where])
                self.assertNotIn(CANARY, json.dumps(seen[where]))
                self.assertEqual(seen[where].get("PATH"), launch["PATH"])


if __name__ == "__main__":
    unittest.main()
