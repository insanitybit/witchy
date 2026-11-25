"""Operator record transitions after Coven verifies a snapshot. SnapshotRun exercises signed bytes."""
import contextlib
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from registry_run import load_cli


class Record(unittest.TestCase):
    honest = "witchy-2:" + "a" * 96
    writer = "witchy-2:" + "b" * 96

    def setUp(self):
        self.cli = load_cli()
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.enterContext(patch.dict(os.environ, XDG_STATE_HOME=str(self.root / "state")))

    def observe(self, signer, anchor):
        registry = self.cli.Registry(self.root / "registry", False, operator=anchor,
                                     registry_id="test" if anchor else None)
        registry.trust_first_use = anchor is None
        kept = self.root / "snapshot"
        store = self.cli.TrustStore(registry)
        try:
            with patch.object(self.cli, "coven_snapshot", return_value=(1, "test", [signer])) as verified:
                result = self.cli.verify_snapshot(registry, None, store, self.root)
                verified.assert_called_once_with(registry, anchor, registry.registry_id, 0, kept)
                self.assertEqual(result, (1, signer, kept, "test"))
            store.save()
        finally:
            store.close()
        reopened = self.cli.TrustStore(registry)
        try:
            return reopened.operator
        finally:
            reopened.close()


class OperatorRecord(Record):
    def test_project_anchor_creates_a_provisional_record(self):
        record = self.observe(self.honest, self.honest)
        self.assertEqual((record["keys"], record["source"], record["provisional"]),
                         ([self.honest], "project", True))

    def test_project_anchor_does_not_promote_a_first_use(self):
        first = self.observe(self.honest, None)
        self.assertEqual((first["keys"], first["source"]), ([self.honest], "first-use"))
        self.assertIs(self.observe(self.honest, self.honest)["provisional"], True)

    def test_replacing_a_first_use_does_not_create_a_trusted_record(self):
        self.assertEqual(self.observe(self.writer, None)["keys"], [self.writer])
        record = self.observe(self.honest, self.honest)
        self.assertEqual((record["keys"], record["source"], record["provisional"]),
                         ([self.honest], "project", True))


class TrustCommand(Record):
    """`grimoire trust` against a record this machine holds: a refused call changes nothing."""
    retired = "witchy-2:" + "c" * 96
    unknown = "witchy-2:" + "d" * 96

    def setUp(self):
        super().setUp()
        self.observe(self.honest, self.honest)
        # A record whose earlier key handed the role on (signs nothing until reinstated) and whose
        # writer key is unestablished (named by a record without provenance).
        registry = self.cli.Registry(self.root / "registry", False, operator=self.honest, registry_id="test")
        store = self.cli.TrustStore(registry)
        try:
            store.operator = dict(store.operator, handed_on=[self.retired], unestablished=[self.writer], handover={self.retired: [self.honest]})
            store.save()
            self.record = store.path
        finally:
            store.close()
        self.before = self.record.read_bytes()
        self.assertEqual(self.held()["handed_on"], [self.retired])
        self.assertEqual(self.held()["unestablished"], [self.writer])
        self.app = self.root / "app"
        self.app.mkdir()
        (self.app / "witchy.toml").write_text(f'[registry]\npath = "../registry"\noperator = "{self.honest}"\nid = "test"\n\n'
                                              '[dependencies]\nlib = "1.0.0"\n')

    def held(self):
        import json
        return json.loads(self.record.read_text())["operator"]

    def trust(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = self.cli.cli_main(["trust", str(self.app), *args], {})
        return status, out.getvalue(), err.getvalue()

    def refused(self, message, *args):
        status, out, err = self.trust(*args)
        self.assertEqual((status, out), (1, ""), err)
        self.assertIn(message, err)
        self.assertEqual(self.record.read_bytes(), self.before)

    def test_the_command_reads_this_record(self):
        status, out, err = self.trust()
        self.assertEqual(status, 0, err)
        self.assertIn(f"{self.retired} sign nothing", out)

    def test_reinstating_a_key_not_handed_on_is_refused(self):
        self.refused("holds no operator key by that ID refused for signing", "--reinstate-signer", self.honest)

    def test_reinstating_an_unknown_key_is_refused(self):
        self.refused("holds no operator key by that ID refused for signing", "--reinstate-signer", self.unknown)

    def test_a_refused_reinstate_saves_nothing_of_the_same_calls_reestablish(self):
        """`--reestablish-operator` is applied first in memory; the call's refused reinstate must
        leave the writer key unestablished on disk."""
        for key in (self.honest, self.unknown):
            with self.subTest(key=key):
                self.refused("holds no operator key by that ID refused for signing",
                             "--reestablish-operator", self.writer, "--reinstate-signer", key)
                self.assertEqual(self.held()["unestablished"], [self.writer])

    def test_reestablish_or_reinstate_with_a_forget_is_refused_whole(self):
        """Each returned after its own change, so a forget in the same call was silently not done."""
        separate = "are separate acts, one per call; nothing changed"
        for args in (["--reestablish-operator", self.writer, "--forget-operator"],
                     ["--reinstate-signer", self.retired, "--forget-operator"],
                     ["--reestablish-operator", self.writer, "--forget", "lib"],
                     ["--reestablish-operator", self.retired, "--reinstate-signer", self.retired, "--forget", "lib"],
                     ["--reinstate-signer", self.retired, "--forget-unkeyed-floor"],
                     ["--forget-operator", "--forget", "lib"],
                     ["--forget-unkeyed-floor", "--forget-operator"]):
            with self.subTest(args=args):
                self.refused(separate, *args)
        # Alone, each still acts.
        status, out, err = self.trust("--reestablish-operator", self.writer)
        self.assertEqual(status, 0, err)
        self.assertNotIn(self.writer, self.held()["unestablished"])


if __name__ == "__main__":
    unittest.main()
