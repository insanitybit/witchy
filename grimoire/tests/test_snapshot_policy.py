"""Operator trust decisions over verified snapshot facts. These cases retain real lock and
trust-store persistence, including migration of records from older Grimoire versions."""
import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from registry_run import load_cli
from test_trust_policy import key, RECORD, DIGEST


class SnapshotPolicy(unittest.TestCase):
    operator = key("operator")

    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.app = self.root / "consumer/app"
        self.app.mkdir(parents=True)
        (self.app / "witchy.toml").write_text(
            f'[registry]\npath = "../../registry"\noperator = "{self.operator}"\nid = "test"\n'
            '[dependencies]\nlib = "1.0.0"\n')
        self.enterContext(patch.dict(os.environ, XDG_STATE_HOME=str(self.root / "state")))
        self.cli = load_cli()
        self.served = (self.operator, 0)
        self.links = []
        self.wire_snapshot = self.cli.coven_snapshot
        self.cli.coven_snapshot = self.verified
        self.wire_fetch = self.cli.coven_fetch
        self.cli.coven_fetch = self.fetched

    def verified(self, registry, anchor, registry_id, floor, keep, quiet=False):
        signer, sequence = self.served
        chain = [old for old, new in self.links] + [signer]
        self.assertEqual([new for old, new in self.links], chain[1:])
        self.assertIn(registry_id, (None, "test"))
        # Supply the response for the requested anchor, as the signed HTTP witness does.
        if anchor and anchor not in chain:
            raise self.cli.ProjectError("snapshot does not include the requested operator")
        if sequence < floor:
            raise self.cli.ProjectError(f"the registry serves snapshot {sequence}, older than snapshot {floor} already seen")
        return sequence, "test", chain[chain.index(anchor):] if anchor else chain

    def fetched(self, registry, name, version, into, *pins):
        self.assertEqual((name, version), ("lib", "1.0.0"))
        into.mkdir(parents=True)
        (into / "package").write_bytes(RECORD)
        (into / "source.wy").write_bytes(b"source")
        facts = self.cli.Fetched()
        facts.ok, facts.digest = True, DIGEST
        return facts

    def snapshot(self):
        self.served = self.served[0], self.served[1] + 1

    def writer_snapshot(self, writer, sequence):
        previous = self.served
        self.served = writer, sequence
        return previous

    def rotate(self, previous, successor):
        self.links = self.links + [(previous, successor)]
        self.served = successor, self.served[1] + 1

    def decide(self, action, project, *, first_use=False, env=None, **options):
        project = self.root / project
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), patch.dict(os.environ, env or {}):
            try:
                if action == "lock":
                    self.cli.lock_project(project, trust_first_use=first_use)
                    status = 0
                elif action == "trust":
                    self.cli.trust_project(project, **options)
                    status = 0
                else:
                    assert action == "frozen"
                    registry, deps = self.cli.read_dependencies(project / "witchy.toml")
                    with tempfile.TemporaryDirectory(dir=self.root) as staging:
                        self.cli.read_lock(project / "witchy.toml", deps, registry, staging)
                    status = 0
            except self.cli.ProjectError as error:
                print(error, file=err)
                status = 1
        return subprocess.CompletedProcess([action], status, out.getvalue(), err.getvalue())

    def refused(self, result, reason):
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn(reason, result.stderr)

    def operator_record(self):
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        return json.loads(store.read_text())["operator"]

    def undeclared_project(self):
        """A copy of the app that declares no operator: its lock is a first use."""
        project = self.root / "consumer/undeclared"
        shutil.copytree(self.app, project)
        manifest = project / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{self.operator}"\nid = "test"\n', ""))
        return "consumer/undeclared"

    def cloned_project(self, key_print):
        """A copy of the app whose witchy.toml declares `key_print` as the operator (a project
        someone else wrote, cloned here)."""
        project = self.root / "consumer/clone"
        shutil.copytree(self.app, project)
        manifest = project / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{self.operator}"', f'operator = "{key_print}"'))
        return "consumer/clone"

    def floors(self):
        return self.operator_record()["floors"]

    def test_floors_are_kept_per_operator_key(self):
        """This machine keeps a snapshot floor per operator key. A cloned project declares a
        registry writer's key, which serves a snapshot at 2^40, replacing a first use of the
        honest operator: the writer's floor binds no chain without the writer's key, so after
        `--forget-operator` your own project locks (adversarial review of e2280af: one floor for
        the machine stayed at 2^40 for good). The honest key's floor survives the replacement and
        the forget, and still refuses an older honest snapshot (review of the fix)."""
        writer = key("writer")
        self.snapshot()
        older = self.served
        self.snapshot()
        undeclared = self.decide("lock", self.undeclared_project(), first_use=True)
        self.assertEqual(undeclared.returncode, 0, undeclared.stderr)
        self.assertEqual(self.floors(), {self.operator: 2})
        honest = self.writer_snapshot(writer, 2 ** 40)
        cloned = self.decide("lock", self.cloned_project(writer))
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        self.assertEqual((self.operator_record()["keys"], self.floors()), ([writer], {self.operator: 2, writer: 2 ** 40}))
        self.served = honest
        self.refused(self.decide("lock", "consumer/app"), "a fork or a rollback")
        forgot = self.decide("trust", "consumer/app", forget_operator=True)
        self.assertIn("kept each operator key's snapshot floor", forgot.stdout, forgot.stderr)
        self.assertEqual(self.floors(), {self.operator: 2, writer: 2 ** 40})
        self.served = older
        self.refused(self.decide("lock", "consumer/app"), "serves snapshot 1, older than snapshot 2 already seen")
        self.served = honest
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.operator_record()["keys"], [self.operator])

    def test_a_stolen_retired_key_poisons_no_floor_of_the_honest_chain(self):
        """The old operator key, handed on to a successor and stolen, serves a snapshot at 2^40:
        through a fork to a thief, or signed itself with the rotations deleted. A project declaring
        the old key locks on it, on a machine with no record. A snapshot is checked against its
        signer's floor only, and only that floor rises, so after `--forget-operator` your project's
        honest chain locks: through the old key, or from the current operator where this machine saw
        the old key hand the role to the thief. Before, the old key's floor rose with the
        fork's (review of 6b97913), or bound the successor's snapshots (review of 7cfc468)."""
        self.snapshot()
        successor = key("operator-2")
        thief = key("thief")
        self.rotate(self.operator, successor)
        rotations, honest = self.links, self.served
        planted = lambda key: (key, 2 ** 40)
        fork = [(self.operator, thief)]
        for served_rotations, snapshot, poisoned in [(fork, planted(thief), thief), (None, planted(self.operator), self.operator)]:
            for store in (self.root / "state/grimoire/trust").glob("*.json"):
                store.unlink()  # a machine with no record
            if served_rotations is None:
                self.links = []
            else:
                self.links = served_rotations
            self.served = snapshot
            shutil.rmtree(self.root / "consumer/clone", ignore_errors=True)
            forked = self.decide("lock", self.cloned_project(self.operator))
            self.assertEqual(forked.returncode, 0, forked.stderr)
            self.assertEqual(self.floors(), {poisoned: 2 ** 40})
            self.links = rotations
            self.served = honest
            self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
            project = "consumer/app"
            if poisoned == thief:
                # A cloned project's chain fixes no successor (review of 7228c51): the old key was
                # only before the record, and after the forget hands the role to no one, so your
                # project declares the current operator (review of ef0fdb0). Before, the thief was
                # fixed as its successor ("a fork by a stolen key").
                self.refused(self.decide("lock", project), "on to a successor it no longer records")
                shutil.rmtree(self.root / "consumer/clone")
                project = self.cloned_project(successor)
            locked = self.decide("lock", project)
            self.assertEqual(locked.returncode, 0, locked.stderr)
            self.assertEqual(self.floors(), {poisoned: 2 ** 40, successor: 2})
            (self.root / project / "witchy.lock").unlink()

    def test_a_record_without_a_source_migrates_as_forgotten_with_its_floor(self):
        """A record d028c5c or 101f2b4 wrote has no `source`, marked a project's record
        `provisional` too, and kept one floor (`snapshot`): it migrates as forgotten, never as a
        first use (adversarial review of e2280af: a cloned project's writer key, migrated so, was
        what `--trust-first-use` anchored on), and its floor moves onto its keys, still refusing
        an older snapshot (review of the fix)."""
        writer = key("writer")
        self.snapshot()
        older = self.served
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()  # so only this machine's floor refuses below
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        data = json.loads(store.read_text())
        self.assertEqual(data["operator"].pop("source"), "project")
        data["operator"]["snapshot"] = data["operator"].pop("floors")[self.operator]
        store.write_text(json.dumps(data))
        honest = self.writer_snapshot(writer, 3)
        refused = self.decide("lock", self.undeclared_project(), first_use=True)
        self.refused(refused, "this machine forgot the registry's operator")
        self.assertIn("set aside as forgotten", refused.stderr)
        self.assertEqual((self.operator_record()["keys"], self.floors()), ([], {self.operator: 2}))
        self.served = older
        self.refused(self.decide("lock", "consumer/app"), "serves snapshot 1, older than snapshot 2 already seen")
        self.served = honest
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)

    def test_a_record_forgotten_before_the_upgrade_keeps_its_floor_unkeyed(self):
        """e2280af and earlier kept the single floor (`snapshot`) through `--forget-operator`, with
        no keys. Migrated onto the record's keys, it was lost, and an older snapshot was let through.
        It is kept unkeyed: it binds every chain until the user drops it explicitly."""
        self.snapshot()
        older = self.served
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()  # so only this machine's floor refuses below
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        data = json.loads(store.read_text())
        data["operator"] = {"keys": [], "retired": [], "snapshot": 2, "provisional": True, "source": "first-use", "registry": None}
        store.write_text(json.dumps(data))
        honest = self.served
        self.served = older
        self.refused(self.decide("lock", "consumer/app"), "serves snapshot 1, older than snapshot 2 this machine saw before its operator record was forgotten")
        self.served = honest
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual((self.floors(), self.operator_record()["unkeyed"]), ({self.operator: 2}, 2))
        dropped = self.decide("trust", "consumer/app", forget_unkeyed_floor=True)
        self.assertIn("dropped the unkeyed snapshot floor (snapshot 2)", dropped.stdout, dropped.stderr)
        self.assertEqual(self.operator_record()["unkeyed"], 0)

    def test_a_retired_key_does_not_fork_a_first_use_under_a_declaration(self):
        """This machine trusted A -> B on first use, so A is retired. A thief with A hands the role
        to X and signs as X; a project declaring A must not make that fork the trusted record
        (review of d902ac6: the anchor replaced the first use whenever B was missing)."""
        self.snapshot()
        successor = key("operator-2")
        thief = key("thief")
        self.rotate(self.operator, successor)
        fresh = self.root / "consumer/fresh"
        shutil.copytree(self.app, fresh)
        manifest = fresh / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{self.operator}"\nid = "test"\n', ""))
        self.assertEqual(self.decide("lock", "consumer/fresh", first_use=True).returncode, 0)
        self.links = [(self.operator, thief)]
        self.served = (thief, 50)
        self.refused(self.decide("lock", "consumer/app"), "does not continue the one this machine verified")
        self.assertNotIn(thief, self.decide("trust", "consumer/app").stdout)

    def test_a_fork_of_the_operator_chain_is_refused(self):
        """The registry serves a shorter or other operator chain: the stolen old key hands the role
        to a thief instead of the successor this machine verified. This machine keeps the longest
        chain it verified and refuses one that does not extend it (automated review of c00d6a6)."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        successor = key("operator-2")
        thief = key("thief")
        self.rotate(self.operator, successor)
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        self.links = [(self.operator, thief)]
        self.served = (thief, 50)
        (self.app / "witchy.lock").unlink()
        self.refused(self.decide("lock", "consumer/app"), f"signed by {thief} by an operator chain that does not continue the one this machine verified")

    def test_a_cloned_projects_earlier_links_leave_a_first_use_as_it_was(self):
        """This machine trusted the operator A on first use. A cloned project declares a registry
        writer's key W, and the writer serves W -> A beside A's honest snapshot. The project
        locks, but its earlier link verifies for that resolve only: the record keeps [A] and stays
        a first use, so an honest `--trust-first-use` still locks (review of ef0fdb0: W was
        prepended and the record became the project's)."""
        writer = key("writer")
        self.snapshot()
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = [(writer, self.operator)]
        cloned = self.decide("lock", self.cloned_project(writer))
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        self.links = []
        (self.root / "consumer/undeclared/witchy.lock").unlink()
        relocked = self.decide("lock", "consumer/undeclared", first_use=True)
        self.assertEqual(relocked.returncode, 0, relocked.stderr)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["source"], record["retired"], record["before"]), ([self.operator], "first-use", {}, {writer: [self.operator]}))

    def test_a_link_before_a_first_use_still_retires_its_key(self):
        """A -> B, and this machine's first use, served without the rotation, starts at B. Your
        project declaring A verifies A -> B, and the record keeps [B]; A is kept as before the
        record, so a stolen A handing the role to a thief is refused as a fork rather than replacing
        the record (review of 40b6650). It fixes no successor of A (review of 83604b5)."""
        self.snapshot()
        successor = key("operator-2")
        thief = key("thief")
        self.rotate(self.operator, successor)
        rotations, honest = self.links, self.served
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotations
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.links = [(self.operator, thief)]
        self.served = (thief, 50)
        (self.app / "witchy.lock").unlink()
        self.refused(self.decide("lock", "consumer/app"), f"signed by {thief} by an operator chain that does not continue the one this machine verified")
        record = self.operator_record()
        self.assertEqual((record["keys"], record["source"], record["retired"], record["before"]), ([successor], "first-use", {}, {self.operator: [successor]}))

    def test_a_cloned_projects_links_before_a_first_use_retire_no_key(self):
        """A -> B honestly, and this machine's first use started at B. A cloned project declares A,
        and a thief holding A serves A -> T -> B beside B's honest snapshot: it locks, and recorded
        A's successor as T (and T's as B), so your own projects declaring A were refused as a fork
        by a stolen key, past `--forget-operator` (review of 83604b5). Links before the record's
        first key fix no successor: A and T are only before the record."""
        thief = key("thief")
        self.snapshot()
        successor = key("operator-2")
        self.rotate(self.operator, successor)
        rotations = self.links
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.assertEqual(self.operator_record()["keys"], [successor])
        self.links = [(self.operator, thief)] + [(thief, successor)]
        cloned = self.decide("lock", self.cloned_project(self.operator))
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["source"], record["retired"]), ([successor], "first-use", {}))
        self.assertEqual(record["before"], {self.operator: [thief, successor], thief: [successor]})
        self.links = rotations
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.operator_record()["retired"], {})
        # After `--forget-operator` A and T hand the role to no one (review of 5f467cd): no
        # successor of theirs was ever fixed, so a project declares the current operator.
        (self.app / "witchy.lock").unlink()
        self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
        self.assertEqual(self.operator_record()["retired"], {self.operator: None, thief: None})
        self.refused(self.decide("lock", "consumer/app"), "on to a successor it no longer records")
        manifest = self.app / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{self.operator}"', f'operator = "{successor}"'))
        relocked = self.decide("lock", "consumer/app")
        self.assertEqual(relocked.returncode, 0, relocked.stderr)

    def test_a_key_before_a_first_use_hands_the_role_to_no_one_once_the_record_goes(self):
        """A -> B, a first use from B, and your project's A -> B: A is before the record. Once the
        record goes, by a replace (a cloned project's writer key W with its own snapshot), A stays
        before the record, and by `--forget-operator`, A retires with an unknown successor: a
        stolen A handing the role to a thief is refused (review of 5f467cd: `before` was
        dropped, and A handed the role on)."""
        writer = key("writer")
        thief = key("thief")
        self.snapshot()
        successor = key("operator-2")
        self.rotate(self.operator, successor)
        rotations, honest = self.links, self.served
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotations
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["before"], record["retired"]), ([successor], {self.operator: [successor]}, {}))
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        kept = store.read_text()
        stolen_rotation = [(self.operator, thief)]
        stolen_snapshot = (thief, 50)
        # The record goes by a replace: a cloned project declares W, served W's own snapshot.
        self.links = []
        self.writer_snapshot(writer, 60)
        clone = self.cloned_project(writer)
        (self.root / clone / "witchy.lock").unlink()
        cloned = self.decide("lock", clone)
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["before"], record["retired"]), ([writer], {self.operator: [successor]}, {}))
        # ... or by `--forget-operator`. Either way, a stolen A hands the role to no one.
        for gone in ("replace", "forget"):
            if gone == "forget":
                store.write_text(kept)
                self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
                self.assertEqual((self.operator_record()["before"], self.operator_record()["retired"]), ({}, {self.operator: None}))
            else:
                self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
            self.links = stolen_rotation
            self.served = stolen_snapshot
            (self.app / "witchy.lock").unlink(missing_ok=True)
            self.refused(self.decide("lock", "consumer/app"), f"has {self.operator} hand the role on to a key this machine never verified, and this machine saw it hand the role on to a successor it no longer records")

    def test_a_replace_by_the_current_operator_leaves_projects_declaring_a_key_before_it_locking(self):
        """A -> B -> C honestly, a first use from B, and your project's A -> B: A is before the
        record. A project declaring C, a cloned one or your own, replaces the record. Your
        projects declaring A still run and lock: A stays before the record rather than retiring
        with an unknown successor (review of 88a58ca)."""
        self.snapshot()
        b = key("operator-2")
        self.rotate(self.operator, b)
        rotations = self.links
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotations
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        c = key("operator-3")
        self.rotate(b, c)
        clone = self.cloned_project(c)
        (self.root / clone / "witchy.lock").unlink()
        cloned = self.decide("lock", clone)
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["before"], record["retired"]), ([c], {self.operator: [b]}, {}))
        ran = self.decide("frozen", "consumer/app")
        self.assertEqual(ran.returncode, 0, ran.stderr)
        (self.app / "witchy.lock").unlink()
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)

    def test_a_key_before_a_standing_record_hands_the_role_on_only_toward_it(self):
        """Y -> A -> B honestly, a first use from B, and a project declaring A: A is before the
        record. A thief holding A serves Y -> A -> T and T's snapshot to a project declaring Y,
        which is not before the record, so its chain would replace it: A hands the role to no one
        there (review of 88a58ca: on 5f467cd the record became [Y, A, T])."""
        thief = key("thief")
        self.snapshot()
        a = key("operator-a")
        self.rotate(self.operator, a)
        rotation = self.links
        b = key("operator-b")
        self.rotate(a, b)
        rotations, honest = self.links, self.served
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotations
        clone = self.cloned_project(a)
        (self.root / clone / "witchy.lock").unlink(missing_ok=True)
        locked = self.decide("lock", clone)
        self.assertEqual(locked.returncode, 0, locked.stderr)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["before"], record["retired"]), ([b], {a: [b]}, {}))
        self.links = rotation + [(a, thief)]
        self.served = (thief, 50)
        (self.app / "witchy.lock").unlink(missing_ok=True)
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, f"has {a} hand the role on to a key this machine's record does not hold, and this machine saw it hand the role on to a successor it no longer records")
        self.assertNotIn(thief, refused.stderr)  # the refused chain's successor, unverified here
        self.assertEqual(self.operator_record(), record)

    def test_a_key_before_a_replaced_record_hands_the_role_into_no_other_record(self):
        """A -> B, a first use from B, and your project's A -> B: A is before [B]. A cloned project
        declaring a writer's key W, with W's own snapshot, replaces the record with [W]. A thief
        holding A then serves A -> W: A hands the role only to a key of the record it was before,
        so your projects declaring A are refused rather than made W's (review of d439eca)."""
        writer = key("writer")
        self.snapshot()
        successor = key("operator-2")
        self.rotate(self.operator, successor)
        rotations = self.links
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotations
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        self.links = []
        self.writer_snapshot(writer, 60)
        clone = self.cloned_project(writer)
        (self.root / clone / "witchy.lock").unlink(missing_ok=True)
        cloned = self.decide("lock", clone)
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["before"], record["retired"]), ([writer], {self.operator: [successor]}, {}))
        self.links = [(self.operator, writer)]
        (self.app / "witchy.lock").unlink()
        self.refused(self.decide("lock", "consumer/app"), f"has {self.operator} hand the role to {writer}, and this machine saw it hand the role on to a successor it no longer records")
        self.assertEqual(self.operator_record(), record)

    def test_keys_before_a_first_use_hand_the_role_on_along_their_chain(self):
        """A -> X -> B honestly, and a first use from B. Your project declaring A locks, runs and
        relocks: A may hand the role to X, the next key of its chain, not only to the record's
        (review of c411620: A was kept before [B] alone, and the lock just written was refused)."""
        self.snapshot()
        x = key("operator-x")
        b = key("operator-b")
        self.rotate(self.operator, x)
        self.rotate(x, b)
        rotations = self.links
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotations
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.operator_record()["before"], {self.operator: [x, b], x: [b]})
        ran = self.decide("frozen", "consumer/app")
        self.assertEqual(ran.returncode, 0, ran.stderr)
        (self.app / "witchy.lock").unlink()
        relocked = self.decide("lock", "consumer/app")
        self.assertEqual(relocked.returncode, 0, relocked.stderr)

    def test_a_replaced_record_prepends_no_chain_through_a_key_before_it(self):
        """A -> B -> C honestly, a first use from B, your project's A -> B, and a project declaring C
        replaces the record. A thief holding the old B serves A -> B -> T -> C beside C's honest
        snapshot: no successor is fixed from those links, so your projects declaring A still lock
        on the honest chain, and a forget retires B to no one, not to T (review of c411620: B was
        retired to T, which refused the honest chain past a forget)."""
        thief = key("thief")
        self.snapshot()
        b = key("operator-2")
        self.rotate(self.operator, b)
        rotation = self.links
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotation
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        c = key("operator-3")
        self.rotate(b, c)
        rotations, honest = self.links, self.served
        clone = self.cloned_project(c)
        (self.root / clone / "witchy.lock").unlink(missing_ok=True)
        cloned = self.decide("lock", clone)
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        self.links = rotation + [(b, thief)] + [(thief, c)]
        (self.app / "witchy.lock").unlink()
        self.decide("lock", "consumer/app")
        self.assertNotIn(b, self.operator_record()["retired"])
        self.links = rotations
        self.served = honest
        (self.app / "witchy.lock").unlink(missing_ok=True)
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
        self.assertEqual(self.operator_record()["retired"].get(b), None)

    def test_a_project_record_takes_no_keys_before_its_first(self):
        """A -> B -> C honestly, a first use from B, and a project declaring C replaces the record
        with [C]. A thief holding the old B serves B -> T -> C beside C's honest snapshot to a
        project declaring B (a cloned one, or your own first use's lock): the record takes no keys
        before its first, so B is not retired to T, and your projects declaring B still lock on the
        honest chain (review of 0f9fae6: B -> T was fixed, past a forget)."""
        thief = key("thief")
        self.snapshot()
        b = key("operator-2")
        self.rotate(self.operator, b)
        rotation = self.links
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotation
        c = key("operator-3")
        self.rotate(b, c)
        rotations = self.links
        clone = self.cloned_project(c)
        (self.root / clone / "witchy.lock").unlink(missing_ok=True)
        cloned = self.decide("lock", clone)
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        self.links = rotation + [(b, thief)] + [(thief, c)]
        manifest = self.app / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{self.operator}"', f'operator = "{b}"'))
        (self.app / "witchy.lock").unlink(missing_ok=True)
        self.decide("lock", "consumer/app")
        record = self.operator_record()
        self.assertEqual((record["keys"], record["retired"]), ([c], {}))
        self.links = rotations
        (self.app / "witchy.lock").unlink(missing_ok=True)
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)

    def test_no_projects_chain_fixes_a_successor_on_create_or_replace(self):
        """Decision (review of 7228c51): a stolen key B, never seen retired here, shown in a chain
        that creates the record (a machine with no record) or replaces it (a clone replacing a
        first use), gets no successor fixed: B -> T is only before the record. After a forget your
        project declaring B is refused with the fix named, the current operator's key included,
        and once it declares the current operator and locks again, it works."""
        thief = key("thief")
        self.snapshot()
        b = key("operator-2")
        self.rotate(self.operator, b)
        c = key("operator-3")
        self.rotate(b, c)
        rotations, honest = self.links, self.served
        stolen = [(b, thief)]
        thief_snapshot = (thief, 50)
        manifest = self.app / "witchy.toml"
        declaring_a = manifest.read_text()
        for path in ("create", "replace"):
            for store in (self.root / "state/grimoire/trust").glob("*.json"):
                store.unlink()  # a machine with no record
            shutil.rmtree(self.root / "consumer/clone", ignore_errors=True)
            shutil.rmtree(self.root / "consumer/undeclared", ignore_errors=True)
            manifest.write_text(declaring_a)
            (self.app / "witchy.lock").unlink(missing_ok=True)
            if path == "replace":
                self.links = []
                self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
                self.assertEqual(self.operator_record()["keys"], [c])
            self.links = stolen
            self.served = thief_snapshot
            clone = self.cloned_project(b)
            (self.root / clone / "witchy.lock").unlink(missing_ok=True)
            forked = self.decide("lock", clone)
            self.assertEqual(forked.returncode, 0, forked.stderr)
            record = self.operator_record()
            self.assertEqual((record["keys"], record["before"].get(b)), ([thief], [thief]))
            self.assertNotIn(b, record["retired"])
            self.links = rotations
            self.served = honest
            self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
            self.assertEqual(self.operator_record()["retired"].get(b), None)
            manifest.write_text(declaring_a.replace(f'operator = "{self.operator}"', f'operator = "{b}"'))
            refused = self.decide("lock", "consumer/app")
            self.refused(refused, "Declare the current operator in witchy.toml and lock again: get the current operator's key ID and the registry's identifier from the registry's operator, out of band")
            self.assertNotIn(c, refused.stderr)  # the signer of the refused chain, unverified here
            manifest.write_text(declaring_a.replace(f'operator = "{self.operator}"', f'operator = "{c}"'))
            locked = self.decide("lock", "consumer/app")
            self.assertEqual(locked.returncode, 0, locked.stderr)

    def test_a_refusal_with_no_record_never_names_the_refused_chains_signer(self):
        """After a forget (no key this machine verified), a stolen A, before the record and so
        retired with an unknown successor, hands the role to a thief T, who signs the snapshot.
        The refusal names no key to declare from that chain: T's key ID is not in it, and it says
        to get the current operator from the registry's operator out of band."""
        thief = key("thief")
        self.snapshot()
        b = key("operator-2")
        self.rotate(self.operator, b)
        rotations, honest = self.links, self.served
        self.links = []
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        self.links = rotations
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
        self.links = [(self.operator, thief)]
        self.served = (thief, 50)
        (self.app / "witchy.lock").unlink()
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "from the registry's operator, out of band (not from this registry's files), check them")
        self.assertNotIn(thief, refused.stderr)

    def test_an_unanchored_refusal_offers_no_key_from_the_registrys_files_where_a_record_is_held(self):
        """A thief's key T signs the snapshot, served with no rotations. A project with no anchor is
        refused without T offered to declare: with a first-use record of A, the refusal names A;
        after `--forget-operator`, it names no key and says to get it out of band (review of
        116d62f: T was printed ready to paste, and pasting it locked onto T)."""
        thief = key("thief")
        self.snapshot()
        project = self.undeclared_project()
        self.assertEqual(self.decide("lock", project, first_use=True).returncode, 0)
        (self.root / project / "witchy.lock").unlink()
        self.served = (thief, 50)
        held = self.decide("lock", project, first_use=False)
        self.refused(held, f"This machine's record holds {self.operator} (a first use)")
        self.assertIn(f'operator = "{self.operator}"\n  id = "test"', held.stderr)
        self.assertNotIn(thief, held.stderr)
        self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
        forgotten = self.decide("lock", project, first_use=False)
        self.refused(forgotten, "from the registry's operator, out of band (not from this registry's files)")
        self.assertNotIn(thief, forgotten.stderr)

    def test_every_key_an_unmarked_record_named_is_unestablished_past_a_forget(self):
        """Review of 0620e18. An unmarked record without `source` (before e2280af) had its chain
        dropped by that older migration first, so its keys were not set aside, and a chain through
        one was accepted unpinned. And `--forget-operator` changed the set-aside keys into retired
        ones, whose refusals named them. B, a non-final key of that chain, handed the role on by
        the record's literal meaning: it signs nothing, pinned or not, until the user re-establishes
        it and reinstates it as a signer (review of 05bc660: this test had B sign on a pin alone,
        the weakening)."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        successor = key("operator-2")
        thief_print = key("thief")
        self.rotate(self.operator, successor)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {"keys": [successor, thief_print], "retired": [], "snapshot": 1, "provisional": True, "registry": "test"}}))
        self.assertIn(f"unestablished {successor} {thief_print} (named by a record set aside on upgrade", self.decide("trust", "consumer/app").stdout)
        # Your project declaring A: its chain reaches B, which it does not pin.
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "holds a key named by this machine's operator record from before Grimoire marked where its keys came from, which this project does not pin")
        self.assertFalse(any(key in refused.stderr for key in (successor, thief_print)), refused.stderr)
        # Pinned on B by one project: refused, naming no key, and the pin changes nothing here.
        clone = self.cloned_project(successor)
        pinned = self.decide("lock", clone)
        self.refused(pinned, "A project's pin does not let it sign again; only `grimoire trust --reinstate-signer KEY` does")
        self.assertFalse(any(key in pinned.stderr for key in (successor, thief_print)), pinned.stderr)
        self.assertEqual((self.operator_record()["unestablished"], self.operator_record()["handed_on"]), ([successor, thief_print], [successor]))
        # The user re-establishes B and reinstates it as a signer; then the pin locks, for that
        # resolve only, and after a forget T stays unestablished.
        self.assertEqual(self.decide("trust", "consumer/app", reestablish_operator=successor, reinstate_signer=successor).returncode, 0)
        locked = self.decide("lock", clone)
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.operator_record()["unestablished"], [thief_print])
        self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
        record = self.operator_record()
        self.assertEqual((record["unestablished"], record["retired"], record["before"]), ([thief_print], {}, {}))

    def test_a_projects_pin_establishes_no_key_for_the_machine(self):
        """A record without provenance named A and K. A cloned project pinning K locks on it, for its
        own resolve: a project's anchors never change this machine's trust record, so K stays
        unestablished, and your project pinning A, whose chain A -> K reaches K, is still refused,
        naming neither (review of a9a5b25: the clone's pin cleared K for every project). Only the
        user re-establishes K, with `grimoire trust --reestablish-operator K`."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        k = key("operator-2")
        self.rotate(self.operator, k)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        old = {"keys": [self.operator, k], "retired": {self.operator: k}, "before": {},
               "floors": {self.operator: 1}, "unkeyed": 0, "provisional": True, "source": "project", "registry": "test"}
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": old}))
        cloned = self.decide("lock", self.cloned_project(k))
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        self.assertEqual((self.operator_record()["keys"], self.operator_record()["unestablished"]), ([k], [self.operator, k]))
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "which this project does not pin")
        self.assertIn("grimoire trust --reestablish-operator KEY", refused.stderr)
        self.assertFalse(any(key in refused.stderr for key in (self.operator, k)), refused.stderr)
        unanchored = self.decide("lock", self.undeclared_project())
        self.refused(unanchored, "This machine holds no operator key it can name")
        self.assertNotIn(k, unanchored.stderr)
        # The user, having checked K with the registry's operator, re-establishes it.
        self.refused(self.decide("trust", "consumer/app", reestablish_operator="witchy-2:" + "0" * 96), "holds no unestablished operator key by that ID")
        done = self.decide("trust", "consumer/app", reestablish_operator=k)
        self.assertIn(f"re-established {k}", done.stdout, done.stderr)
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.operator_record()["unestablished"], [self.operator])

    def unmarked_thief_chain(self, operator):
        """An unmarked (or interim-marked) record `operator` written here, and a thief holding A
        serving A -> T with T's snapshot; returns T's key ID."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        thief = key("thief")
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": operator(thief)}))
        honest = self.served
        self.links = [(self.operator, thief)]
        self.served = (thief, 60)
        return thief, honest

    def test_a_key_an_unmarked_record_kept_only_a_floor_of_is_unestablished(self):
        """An earlier `--forget-operator` dropped the record's keys and kept their floors: the
        forgotten newest key T survived only as a floor key, was not marked unestablished, and a
        project pinning A was served A -> T and locked on T (review of a9a5b25)."""
        thief_print, _ = self.unmarked_thief_chain(lambda thief: {"keys": [], "retired": {}, "before": {}, "floors": {thief: 50}, "unkeyed": 0,
                                                                    "provisional": True, "source": "first-use", "registry": None})
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "which this project does not pin")
        self.assertNotIn(thief_print, refused.stderr)
        self.assertEqual(self.operator_record()["unestablished"], [thief_print])

    def test_a_record_marked_while_the_decision_was_settled_keeps_its_retirements(self):
        """Records marked 1 (0620e18 to 112159d), where a forget retired set-aside keys with no
        successor: no load weakens a constraint this machine recorded (review of a7a0169: they
        were moved to unestablished, which a pin lifts). Both stay retired, handing the role to
        no one, and a key set aside with none to hand the role to retires too."""
        thief_print, honest = self.unmarked_thief_chain(lambda thief: {"keys": [], "retired": {self.operator: None, thief: None}, "before": {}, "floors": {self.operator: 1},
                                                                      "unkeyed": 0, "provisional": True, "source": "first-use", "registry": None, "provenance": 1})
        self.refused(self.decide("lock", "consumer/app"), "a key this machine saw hand the operator role on")
        self.links = []
        self.served = honest
        self.refused(self.decide("lock", "consumer/app"), "a key this machine saw hand the operator role on")
        record = self.operator_record()
        self.assertEqual((record["retired"], record["provenance"]), ({self.operator: None, thief_print: None}, 2))
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**record, "retired": {}, "before": {thief_print: []}, "provenance": 1}}))
        self.decide("trust", "consumer/app")
        self.assertEqual((self.operator_record()["retired"], self.operator_record()["before"]), ({thief_print: None}, {}))

    def test_a_record_marked_1_keeps_its_retirements_against_a_pin(self):
        """A record marked 1 was written under e77e23b's rules: A retired to B there is this
        machine's own verified fact. Migrated whole to unestablished, a cloned project pinning A,
        served A -> T by a thief holding A, locked on T (review of bca4a36). Its chain and known
        retirements are kept, and a pin never re-accepts a retired key."""
        b_print = key("operator-2")
        thief_print, _ = self.unmarked_thief_chain(lambda thief: {"keys": [b_print], "retired": {self.operator: b_print}, "before": {}, "floors": {b_print: 2},
                                                                    "unkeyed": 0, "provisional": True, "source": "project", "registry": "test", "unestablished": [], "provenance": 1})
        refused = self.decide("lock", self.cloned_project(self.operator))
        self.refused(refused, "a fork or a rollback")
        record = self.operator_record()
        self.assertEqual((record["keys"], record["retired"], record["unestablished"], record["provenance"]),
                         ([b_print], {self.operator: b_print}, [], 2))

    def test_an_unestablished_key_fixes_no_successor_even_pinned(self):
        """Review of 112159d. A project pinning A, unestablished, starts a record on A; the
        operator's A -> B then extended it and fixed A's successor as B for the machine, from one
        project's pin (perhaps a cloned one's)."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {"keys": [self.operator], "retired": {}, "before": {}, "floors": {self.operator: 1},
                                                                                   "unkeyed": 0, "provisional": True, "source": "project", "registry": "test"}}))
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        b = key("operator-2")
        self.rotate(self.operator, b)
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["retired"], record["unestablished"]), ([self.operator, b], {}, [self.operator]))

    def test_the_anchor_refusal_names_no_unestablished_signer(self):
        """Review of bca4a36. witchy.toml pins A and the lock B; a thief holding A, both named by an
        unmarked record, serves A -> T. The anchor check ran first and named T as the signer."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        b = key("operator-2")
        self.rotate(self.operator, b)
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        self.assertIn(f'operator = "{b}"', (self.app / "witchy.lock").read_text())
        thief = key("thief")
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {"keys": [self.operator, thief], "retired": {self.operator: thief}, "before": {},
                                                                                   "floors": {self.operator: 1}, "unkeyed": 0, "provisional": True, "source": "project", "registry": "test"}}))
        self.links = [(self.operator, thief)]
        self.served = (thief, 60)
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "which this project does not pin")
        self.assertNotIn(thief, refused.stderr)

    def test_a_marked_retirement_a_pin_fixed_keeps_its_key_retired(self):
        """112159d fixed A's successor from a pin of unestablished A, so a record marked 1 may hold
        A retired to T with A unestablished. T is never a fixed successor, and A stays retired,
        handing the role to no one, past a forget: no load weakens a constraint (reviews of
        3f1ff11 and a7a0169). The refusal names neither."""
        thief_print = key("thief")
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {
            "keys": [self.operator, thief_print], "retired": {self.operator: thief_print}, "before": {}, "floors": {self.operator: 1},
            "unkeyed": 0, "provisional": True, "source": "project", "registry": "test", "unestablished": [self.operator], "provenance": 1}}))
        self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
        self.assertEqual(self.operator_record()["retired"], {self.operator: None})
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "signed by a key, a key this machine saw hand the operator role on")
        self.assertFalse(any(key in refused.stderr for key in (self.operator, thief_print)), refused.stderr)

    def test_a_marked_retirement_of_an_unestablished_key_is_never_deleted(self):
        """Review of a7a0169: loading a marked record deleted the retirement of a key in
        `unestablished` (A retired to B, A unestablished), so A could sign again for a project
        pinning it. It is kept, with no successor."""
        b_print = key("operator-2")
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {
            "keys": [b_print], "retired": {self.operator: b_print}, "before": {}, "floors": {self.operator: 1},
            "unkeyed": 0, "provisional": True, "source": "project", "registry": "test", "unestablished": [self.operator], "provenance": 2}}))
        self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
        self.assertEqual(self.operator_record()["retired"], {self.operator: None})
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "signed by a key, a key this machine saw hand the operator role on")
        self.assertNotIn(self.operator, refused.stderr)

    def test_no_successor_is_fixed_onto_an_unestablished_key(self):
        """Review of 3f1ff11. A cloned project pinning A in witchy.toml and T, unestablished, in its
        lock, served A -> T by a thief holding A, fixed A's successor as T for the machine, and a
        later refusal named T."""
        thief = key("thief")
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        record = self.operator_record()
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**record, "unestablished": [thief]}}))
        self.links = [(self.operator, thief)]
        self.served = (thief, 60)
        clone = self.cloned_project(thief)
        (self.root / clone / "witchy.lock").unlink(missing_ok=True)
        elsewhere = dict(os.environ, XDG_STATE_HOME=str(self.root / "machine-other"))
        self.assertEqual(self.decide("lock", clone, env=elsewhere).returncode, 0)
        self.assertIn(f'operator = "{thief}"', (self.root / clone / "witchy.lock").read_text())
        manifest = self.root / clone / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{thief}"', f'operator = "{self.operator}"'))
        locked = self.decide("lock", clone)
        self.assertEqual(locked.returncode, 0, locked.stderr)
        # A handed the role on (it signed so), to a key only a pin accepted: retired, no successor
        # (review of d52db7b: unrecorded, a thief's A -> X passed after a forget).
        self.assertEqual(self.operator_record()["retired"], {self.operator: None})
        self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
        x = key("operator-3")
        self.links = [(self.operator, x)]
        self.served = (x, 70)
        (self.app / "witchy.lock").unlink(missing_ok=True)
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "on to a successor it no longer records")
        self.assertFalse(any(key in refused.stderr for key in (thief, x)), refused.stderr)

    def test_a_forget_retires_an_unestablished_key_before_the_record_unnamed(self):
        """A cloned project pinning unestablished A, served A -> T by a thief, leaves A before the
        record [T]. `--forget-operator` keeps the constraint (review of a7a0169: it dropped it, and
        A was only unestablished): A retires with no successor, named neither by the forget nor by
        the refusal (review of d52db7b)."""
        thief_print, honest = self.unmarked_thief_chain(lambda thief: {"keys": [self.operator], "retired": {}, "before": {}, "floors": {self.operator: 1},
                                                                      "unkeyed": 0, "provisional": True, "source": "project", "registry": "test"})
        cloned = self.decide("lock", self.cloned_project(self.operator))
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        self.assertEqual((self.operator_record()["keys"], self.operator_record()["before"]), ([thief_print], {self.operator: [thief_print]}))
        forgot = self.decide("trust", "consumer/app", forget_operator=True)
        self.assertEqual(forgot.returncode, 0, forgot.stderr)
        self.assertIn("1 key(s) a record without provenance named, which handed the role on with no successor recorded, now hand the role to no one", forgot.stdout)
        self.assertNotIn(self.operator, forgot.stdout)
        record = self.operator_record()
        self.assertEqual((record["retired"], record["before"], record["unestablished"]), ({self.operator: None}, {}, [self.operator]))
        self.links = []
        self.served = honest
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "signed by a key, a key this machine saw hand the operator role on")
        self.assertNotIn(self.operator, refused.stderr)

    def test_a_marked_record_keeps_no_retirement_onto_an_unestablished_key(self):
        """Review of d52db7b. bca4a36 to e3688de wrote marker 2 and fixed successors onto
        unestablished keys: a record holding A retired to T, T unestablished, is loaded with A
        retired to no successor, and no refusal names T."""
        thief_print, _ = self.unmarked_thief_chain(lambda thief: {"keys": [self.operator], "retired": {self.operator: thief}, "before": {}, "floors": {self.operator: 1},
                                                                    "unkeyed": 0, "provisional": True, "source": "project", "registry": "test", "unestablished": [thief], "provenance": 2})
        self.assertEqual(self.operator_record()["retired"], {self.operator: thief_print})
        self.decide("trust", "consumer/app")
        self.assertEqual(self.operator_record()["retired"], {self.operator: None})

    def test_a_forget_retires_an_unestablished_keys_recorded_hand_over(self):
        """Review of a7a0169. A project pinning unestablished A starts a record on A, and the honest
        A -> B extends it, fixing no successor. The fork refusal of a thief's A -> X advises a
        forget, after which nothing held A back and A -> X passed. The forget retires A with no
        successor, unnamed. And a record d52db7b wrote, holding A -> T with T unestablished and
        nothing retired, retires A with no successor on load."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {"keys": [self.operator], "retired": {}, "before": {}, "floors": {self.operator: 1},
                                                                                   "unkeyed": 0, "provisional": True, "source": "project", "registry": "test"}}))
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        b = key("operator-2")
        self.rotate(self.operator, b)
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        forgot = self.decide("trust", "consumer/app", forget_operator=True)
        self.assertNotIn(self.operator, forgot.stdout)
        self.assertEqual(self.operator_record()["retired"], {self.operator: None})
        x = key("operator-3")
        self.links = [(self.operator, x)]
        self.served = (x, 70)
        (self.app / "witchy.lock").unlink()
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "on to a successor it no longer records")
        self.assertFalse(any(key in refused.stderr for key in (self.operator, x)), refused.stderr)
        # A record d52db7b wrote: A -> T, T unestablished, nothing retired.
        record = self.operator_record()
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**record, "keys": [b, x], "retired": {}, "unestablished": [x], "provenance": 2}}))
        self.decide("trust", "consumer/app")
        self.assertEqual(self.operator_record()["retired"], {b: None})

    def test_a_marked_record_in_an_older_shape_fixes_no_successor_onto_an_unestablished_key(self):
        """Review of a7a0169: a marked record with a list `retired` went through an older step
        after the cleanup, which fixed A's successor as T, unestablished, and named it. The
        cleanup now runs last: A retires with no successor, and the result is saved."""
        thief_print = key("thief")
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        record = self.operator_record()
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**record, "keys": [self.operator, thief_print], "retired": [self.operator],
                                                                                   "unestablished": [thief_print]}}))
        shown = self.decide("trust", "consumer/app")
        self.assertNotIn(f"(to {thief_print})", shown.stdout)
        self.assertEqual(self.operator_record()["retired"], {self.operator: None})

    def test_a_replace_retires_an_unestablished_keys_hand_over(self):
        """Review of f96d1fe. A first-use record [C, A, B], A unestablished (its hand-over fixed no
        successor), replaced by a cloned project's writer key W: the old keys were dropped, A held
        back by nothing, and after a forget a thief's A -> X passed. The replace retires A with no
        successor. A marked record without `source` keeps its constraints the same way."""
        writer = key("writer")
        a_print = key("operator-2")
        b_print = key("operator-3")
        self.snapshot()
        self.assertEqual(self.decide("lock", self.undeclared_project(), first_use=True).returncode, 0)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        record = self.operator_record()
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**record, "keys": [self.operator, a_print, b_print],
                                                                                   "retired": {self.operator: None}, "unestablished": [a_print]}}))
        self.writer_snapshot(writer, 60)
        cloned = self.decide("lock", self.cloned_project(writer))
        self.assertEqual(cloned.returncode, 0, cloned.stderr)
        record = self.operator_record()
        self.assertEqual((record["keys"], record["retired"].get(a_print, "missing")), ([writer], None))
        # A marked record without `source`: its chain's hand-over and its unestablished keys stay.
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**{k: v for k, v in record.items() if k != "source"},
                                                                                   "keys": [a_print, b_print], "retired": {}, "unestablished": [b_print]}}))
        self.decide("trust", "consumer/app")
        record = self.operator_record()
        self.assertEqual((record["retired"], record["unestablished"]), ({a_print: None}, [b_print]))

    def test_a_malformed_marked_record_without_source_is_refused(self):
        """Review of 40defaf: a marked record without `source` whose `keys` or `before` had the
        wrong type was accepted, and its hand-overs dropped."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        record = {k: v for k, v in self.operator_record().items() if k != "source"}
        for shape in ({"keys": {self.operator: None}}, {"before": self.operator}):
            store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**record, **shape}}))
            self.refused(self.decide("trust", "consumer/app"), "is not a trust store Grimoire wrote")

    def test_a_malformed_before_in_an_unmarked_record_is_refused(self):
        """Review of e33b81e: an unmarked record whose `before` held a key with a value that is not
        a list (a string, null) migrated with no hand-over constraint on that key: fail closed."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        b_print = key("operator-2")
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        kept = json.loads(store.read_text())
        for was in (b_print, None, {b_print: 1}):
            store.write_text(json.dumps({**kept, "operator": {"keys": [b_print], "retired": {}, "before": {self.operator: was}, "floors": {},
                                                              "unkeyed": 0, "provisional": True, "source": "project", "registry": "test"}}))
            self.refused(self.decide("trust", "consumer/app"), "is not a trust store Grimoire wrote")

    def migrated_hand_over(self, successor_name):
        """An unmarked record holding A -> S (S the key `successor_name`, or the thief's), written
        here after the honest rotation A -> B; returns (B, B's key ID, S's key ID)."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        b = key("operator-2")
        s_print = key(successor_name)
        self.rotate(self.operator, b)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {"keys": [self.operator, s_print], "retired": {self.operator: s_print}, "before": {},
                                                                                   "floors": {self.operator: 1}, "unkeyed": 0, "provisional": True, "source": "project", "registry": "test"}}))
        return b, b, s_print

    def test_a_migrated_hand_over_accepts_its_recorded_successor(self):
        """Decision (coordinator): a record without provenance keeps each recorded successor as a
        constraint on where its key may hand the role on, without retiring it for signing. With
        A -> B recorded, a project pinning A (and B, which the old record named too, so a pin is
        what accepts it) locks on the honest A -> B, and the constraint is kept."""
        b, b_print, _ = self.migrated_hand_over("operator-2")
        clone = self.cloned_project(b_print)
        elsewhere = dict(os.environ, XDG_STATE_HOME=str(self.root / "machine-other"))
        self.assertEqual(self.decide("lock", clone, env=elsewhere).returncode, 0)
        manifest = self.root / clone / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{b_print}"', f'operator = "{self.operator}"'))
        locked = self.decide("lock", clone)
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.operator_record()["handover"], {self.operator: [b_print]})

    def test_a_migrated_hand_over_refuses_another_successor_even_pinned(self):
        """With A -> B recorded by a record without provenance, a thief holding A serves A -> T to
        a project pinning A: refused, naming no key (before the decision, the record's successors
        were dropped and the pin of A took A -> T)."""
        b, b_print, _ = self.migrated_hand_over("operator-2")
        thief = key("thief")
        self.links = [(self.operator, thief)]
        self.served = (thief, 60)
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "hand the role on to a key other than the one this machine's operator record from before Grimoire marked where its keys came from recorded")
        self.assertFalse(any(key in refused.stderr for key in (self.operator, b_print, thief)), refused.stderr)

    def test_a_planted_hand_over_leaves_the_current_key_pinnable(self):
        """A record without provenance holds A -> T, which a thief planted. The honest A -> B is
        refused for a project pinning A, which is told to pin the current operator directly,
        naming no key; a project pinning B, the honest current key, locks with no rotation."""
        b, b_print, thief_print = self.migrated_hand_over("thief")
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "Pin the current operator directly: get the current operator's key ID")
        self.assertFalse(any(key in refused.stderr for key in (self.operator, b_print, thief_print)), refused.stderr)
        locked = self.decide("lock", self.cloned_project(b_print))
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.operator_record()["handover"], {self.operator: [thief_print]})

    def test_a_migrated_hand_over_is_each_keys_narrowest_constraint(self):
        """Review of 0e2125b. An unmarked record holding A retired to X and A before the record
        with [X, B] (a replace after a forget) migrated to A -> X or B, a union wider than A's
        retirement: a project pinning A and B locked on A -> B. The retirement binds. And a marked
        record with older-suite keys lost its hand-over constraints in the set-aside step: refused."""
        b = key("operator-2")
        x_print = key("operator-3")
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        (self.app / "witchy.lock").unlink()
        self.rotate(self.operator, b)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {"keys": [b], "retired": {self.operator: x_print}, "before": {self.operator: [x_print, b]},
                                                                                   "floors": {self.operator: 1}, "unkeyed": 0, "provisional": True, "source": "project", "registry": "test"}}))
        self.decide("trust", "consumer/app")
        self.assertEqual(self.operator_record()["handover"][self.operator], [x_print])
        record = self.operator_record()
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**record, "keys": ["witchy-1:" + "0" * 64], "retired": []}}))
        self.refused(self.decide("trust", "consumer/app"), "is not a trust store Grimoire wrote")

    def test_a_migrated_hand_over_is_what_all_its_constraints_allow(self):
        """Review of e33b81e. A key with two disagreeing recorded constraints kept one by priority
        and dropped the other: [A, B] with A retired to X kept only X, and [A, B] with A before the
        record with [X] kept only B, so a project pinning A and B locked on A -> B. The hand-over is
        the intersection of every constraint: none here."""
        b, b_print, x_print = self.migrated_hand_over("operator-3")
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        kept = json.loads(store.read_text())
        for shape in ({"retired": {self.operator: x_print}, "before": {}}, {"retired": {}, "before": {self.operator: [x_print]}}):
            store.write_text(json.dumps({**kept, "operator": {**kept["operator"], "keys": [self.operator, b_print], **shape}}))
            self.decide("trust", "consumer/app")
            self.assertEqual(self.operator_record()["handover"], {self.operator: []})
        store.write_text(json.dumps({**kept, "operator": {**kept["operator"], "keys": [self.operator, b_print], "retired": {}, "before": {self.operator: [x_print]}}}))
        clone = self.cloned_project(b_print)
        elsewhere = dict(os.environ, XDG_STATE_HOME=str(self.root / "machine-other"))
        self.assertEqual(self.decide("lock", clone, env=elsewhere).returncode, 0)
        manifest = self.root / clone / "witchy.toml"
        manifest.write_text(manifest.read_text().replace(f'operator = "{b_print}"', f'operator = "{self.operator}"'))
        refused = self.decide("lock", clone)
        self.refused(refused, "hand the role on to a key other than the one this machine's operator record from before Grimoire marked where its keys came from recorded (none)")
        self.assertFalse(any(key in refused.stderr for key in (self.operator, b_print, x_print)), refused.stderr)

    def test_an_older_suite_record_keeps_its_witchy_2_constraints_and_floors(self):
        """Review of e33b81e. An unmarked record whose keys and retired keys are all of an older
        suite was rebuilt from nothing, dropping its witchy-2 `before` entries (A before the
        record, handing the role only to B), its floor per witchy-2 key and its unkeyed floor. They
        are kept, as for any record without provenance: A and B unestablished, A may hand the role
        only to B. The older suite's keys and floors go: they sign nothing now."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        b_print = key("operator-2")
        witchy1, other1 = "witchy-1:" + "1" * 64, "witchy-1:" + "2" * 64
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {
            "keys": [witchy1], "retired": [other1], "before": {self.operator: [b_print, other1], other1: [witchy1]},
            "floors": {self.operator: 5, witchy1: 9}, "unkeyed": 3, "provisional": False, "source": "project", "registry": "test"}}))
        shown = self.decide("trust", "consumer/app")
        self.assertIn("signed before the witchy-2 profile; set aside as forgotten", shown.stderr)
        record = self.operator_record()
        self.assertEqual({key: record[key] for key in ("keys", "retired", "before", "handover", "unestablished", "floors", "unkeyed", "registry", "provenance")},
                         {"keys": [], "retired": {}, "before": {}, "handover": {self.operator: [b_print]}, "unestablished": [self.operator, b_print],
                          "floors": {self.operator: 5}, "unkeyed": 3, "registry": "test", "provenance": 2})

    def stolen_retired_signer(self):
        """A record without provenance holding the honest A -> B (A retired to B), and a thief
        holding A serving a snapshot signed by A alone above A's floor; returns B's key ID."""
        b, b_print, _ = self.migrated_hand_over("operator-2")
        self.links = []
        self.served = (self.operator, 60)
        return b_print

    def test_a_migrated_retired_key_signs_nothing_even_pinned(self):
        """Review of e33b81e. A record without provenance holds the honest A -> B (A retired to B).
        Migrated to a hand-over constraint alone, A could sign again: a thief holding A served a
        snapshot signed by A alone, at or above A's floor, and a project pinning A took it, where
        the record had refused it as signed by a key seen handing the role on. Decision
        (coordinator): A keeps that refusal, naming no key; a project's pin does not lift it, only
        `grimoire trust --reinstate-signer A` does, once A is established."""
        b_print = self.stolen_retired_signer()
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "saw hand the operator role on. A project's pin does not let it sign again; only `grimoire trust --reinstate-signer KEY` does")
        self.assertIn("that KEY is still its current signing key (not only that it was ever theirs", refused.stderr)
        self.assertFalse(any(key in refused.stderr for key in (self.operator, b_print)), refused.stderr)
        self.assertEqual((self.operator_record()["handed_on"], self.operator_record()["handover"]), ([self.operator], {self.operator: [b_print]}))
        self.assertIn(f"{self.operator} sign nothing", self.decide("trust", "consumer/app").stdout)
        # The user, having checked with the registry's operator that A is still its current
        # signing key, re-establishes A and reinstates it as a signer, in one call.
        done = self.decide("trust", "consumer/app", reestablish_operator=self.operator, reinstate_signer=self.operator)
        self.assertIn(f"reinstated {self.operator} as a signer on this machine", done.stdout, done.stderr)
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        record = self.operator_record()
        self.assertEqual((record["handed_on"], record["handover"]), ([], {self.operator: [b_print]}))

    def test_reestablishing_a_key_leaves_it_refused_for_signing(self):
        """Review of 05bc660: `--reestablish-operator A` also let A sign again, and an operator
        answering "was A ours?" truthfully re-enabled a thief holding A. It lifts only
        `unestablished`; signing needs `--reinstate-signer A`, which refuses a key that is still
        unestablished (fail closed)."""
        self.stolen_retired_signer()
        early = self.decide("trust", "consumer/app", reinstate_signer=self.operator)
        self.refused(early, "that operator key is unestablished on this machine: re-establish it first")
        self.assertEqual(self.operator_record()["handed_on"], [self.operator])
        done = self.decide("trust", "consumer/app", reestablish_operator=self.operator)
        self.assertIn("it still signs nothing until `grimoire trust --reinstate-signer KEY`", done.stdout, done.stderr)
        record = self.operator_record()
        self.assertEqual((self.operator in record["unestablished"], record["handed_on"]), (False, [self.operator]))
        refused = self.decide("lock", "consumer/app")
        self.refused(refused, "A project's pin does not let it sign again")
        self.assertNotIn(self.operator, refused.stderr)
        self.assertEqual(self.decide("trust", "consumer/app", reinstate_signer=self.operator).returncode, 0)
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)

    def test_a_malformed_record_marked_1_is_refused(self):
        """Review of 3f1ff11: a marked-1 record's non-list `unestablished` was dropped silently."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**self.operator_record(), "unestablished": "garbage", "provenance": 1}}))
        self.refused(self.decide("trust", "consumer/app"), "is not a trust store Grimoire wrote")

    def test_an_unanchored_refusal_says_what_to_do_if_a_first_use_records_key_was_stolen(self):
        """The unanchored refusal with a first-use record offers `--trust-first-use` on its key; a
        stolen key is the case a first use is as exposed to as a project's record, so the refusal
        says to forget it and declare the current operator there too (review of 0a97f1d: only a
        project's record was told)."""
        self.snapshot()
        project = self.undeclared_project()
        self.assertEqual(self.decide("lock", project, first_use=True).returncode, 0)
        (self.root / project / "witchy.lock").unlink()
        held = self.decide("lock", project)
        self.refused(held, "or run `grimoire lock --trust-first-use` to anchor on it. If it was stolen, `grimoire trust --forget-operator` and declare the current operator")

    def test_a_retired_key_hands_the_role_only_to_its_successor_past_forget(self):
        """A -> B honestly, seen here. After `--forget-operator`, which a key theft calls for, a
        thief with A hands the role to T, and your project, still declaring A, relocked onto
        [A, T]: only the signer was checked against the retired keys, and the forgotten record had
        no chain to fork from (review of ef0fdb0). Each retired key's successor is kept and binds
        every chain. An earlier store's list of retired keys keeps the successors its chain shows;
        a key whose successor it no longer shows hands the role to no one, and a project then
        declares the current operator."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        successor = key("operator-2")
        thief = key("thief")
        self.rotate(self.operator, successor)
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        record = self.operator_record()
        rotations, honest = self.links, self.served
        self.links = [(self.operator, thief)]
        self.served = (thief, 50)
        (self.app / "witchy.lock").unlink()
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        # Forgotten now, and an earlier Grimoire's record of the same chain (list of retired keys).
        for earlier in (None, {**record, "retired": [self.operator]}):
            if earlier is not None:
                store.write_text(json.dumps({**json.loads(store.read_text()), "operator": earlier}))
            self.assertEqual(self.decide("trust", "consumer/app", forget_operator=True).returncode, 0)
            self.refused(self.decide("lock", "consumer/app"), f"has {self.operator} hand the role on to a key this machine never verified, and this machine saw it hand the role to {successor}")
            self.assertEqual(self.operator_record()["retired"], {self.operator: successor})
        # An earlier Grimoire's record forgotten before the upgrade: A's successor is unknown.
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": {**record, "keys": [], "retired": [self.operator]}}))
        self.refused(self.decide("lock", "consumer/app"), f"has {self.operator} hand the role on to a key this machine never verified, and this machine saw it hand the role on to a successor it no longer records")
        self.links = rotations
        self.served = honest
        self.refused(self.decide("lock", "consumer/app"), "Declare the current operator in witchy.toml and lock again")
        locked = self.decide("lock", self.cloned_project(successor))
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(self.operator_record()["retired"], {self.operator: None})

    def test_an_operator_record_without_provenance_is_pinned_again(self):
        """Before e77e23b a project's chain could fix an operator successor and set a whole key
        chain here: a cloned project declaring A, served A -> T by a thief holding A, left [A, T]
        with A's successor T, and `--forget-operator` kept it, so your projects pinned to A were
        refused for good. Decision (settled with Astra): a record without the provenance marker
        cannot establish where its keys came from, so the operator is pinned again. Its keys and
        successors go, every key it named is unestablished (accepted only where a project pins
        it), its floors stay, the refusals name no key, and the migration runs once. A, which the
        record held retired, signs nothing even pinned (decision, review of e33b81e: it was
        refused there, and a project pinning it locking on it alone was a weakening), until the
        user re-establishes it and reinstates it as a signer; then a project pinning A locks, with
        no rotation."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        thief = key("thief")
        honest = self.served
        (self.app / "witchy.lock").unlink()
        store, = (self.root / "state/grimoire/trust").glob("*.json")
        self.assertEqual(json.loads(store.read_text())["operator"]["provenance"], 2)
        old = {"keys": [self.operator, thief], "retired": {self.operator: thief}, "before": {},
               "floors": {self.operator: 1, thief: 50}, "unkeyed": 1, "provisional": True, "source": "project", "registry": "test"}
        store.write_text(json.dumps({**json.loads(store.read_text()), "operator": old}))
        prints = (self.operator, thief)
        refused = self.decide("lock", self.undeclared_project())
        self.refused(refused, "This machine holds no operator key")
        self.assertIn("written before Grimoire marked where its keys came from; set aside", refused.stderr)
        self.assertFalse(any(key in refused.stderr for key in prints), refused.stderr)
        record = self.operator_record()
        self.assertEqual({key: record[key] for key in ("keys", "retired", "before", "unestablished", "floors", "unkeyed", "provenance")},
                         {"keys": [], "retired": {}, "before": {}, "unestablished": [self.operator, thief],
                          "floors": {self.operator: 1, thief: 50}, "unkeyed": 1, "provenance": 2})
        # Once: the record is saved marked. No first use takes an unestablished key.
        again = self.decide("lock", "consumer/undeclared", first_use=True)
        self.refused(again, "this machine forgot the registry's operator")
        self.assertNotIn("written before Grimoire marked", again.stderr)
        self.assertFalse(any(key in again.stderr for key in prints), again.stderr)
        # The thief's T, which the old record named, is not accepted through A -> T unpinned.
        fork = [(self.operator, thief)]
        stolen = (thief, 60)
        self.links = fork
        self.served = stolen
        through = self.decide("lock", "consumer/app")
        self.refused(through, "which this project does not pin. That record was set aside on upgrade")
        self.assertIn("Pin the operator again: get the current operator's key ID", through.stderr)
        self.assertNotIn(thief, through.stderr)
        # Your project pins the honest current key A, which the record held retired: refused, naming
        # no key, until the user re-establishes A and reinstates it as a signer; then a fresh
        # record on A, no rotation needed.
        self.links = []
        self.served = honest
        pinned = self.decide("lock", "consumer/app")
        self.refused(pinned, "saw hand the operator role on. A project's pin does not let it sign again")
        self.assertFalse(any(key in pinned.stderr for key in prints), pinned.stderr)
        self.assertEqual(self.decide("trust", "consumer/app", reestablish_operator=self.operator, reinstate_signer=self.operator).returncode, 0)
        locked = self.decide("lock", "consumer/app")
        self.assertEqual(locked.returncode, 0, locked.stderr)
        record = self.operator_record()
        self.assertEqual({key: record[key] for key in ("keys", "retired", "before", "unestablished", "handed_on", "floors", "unkeyed", "source")},
                         {"keys": [self.operator], "retired": {}, "before": {}, "unestablished": [thief], "handed_on": [],
                          "floors": {self.operator: 1, thief: 50}, "unkeyed": 1, "source": "project"})
        ran = self.decide("frozen", "consumer/app")
        self.assertEqual(ran.returncode, 0, ran.stderr)
        # Extending from A, T is still refused unless a project pins it.
        self.links = fork
        self.served = stolen
        (self.app / "witchy.lock").unlink()
        self.refused(self.decide("lock", "consumer/app"), "which this project does not pin")
        self.assertEqual(self.operator_record()["keys"], [self.operator])

    def test_a_lock_that_pins_a_package_nothing_requires_is_refused(self):
        """A frozen build takes exactly the graph the manifest requires: a pin beside it (a
        package dropped from [dependencies], or one written in) is refused, not left unread."""
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        lock = self.app / "witchy.lock"
        pinned = lock.read_text()
        start = pinned.index("[[package]]")
        extra = pinned[start:].replace('name = "lib"', 'name = "extra"', 1)
        self.assertIn('name = "extra"', extra)
        lock.write_text(pinned + "\n" + extra)
        self.refused(self.decide("frozen", "consumer/app"), "pins packages the dependencies do not require: run `grimoire lock`")
        lock.write_text(pinned)
        ran = self.decide("frozen", "consumer/app")
        self.assertEqual(ran.returncode, 0, ran.stderr)

    def test_frozen_and_relock_refuse_changed_package_bytes_before_build(self):
        self.snapshot()
        self.refused(self.decide("frozen", "consumer/app"), "run `grimoire lock`")
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        lock = self.app / "witchy.lock"
        before = lock.read_bytes()
        # Use the real wire parser. A refused fetch and a nominally successful
        # response for the wrong digest must both stop resolution.
        self.cli.coven_fetch = self.wire_fetch
        other = "sha384:" + "a"*96
        def reply(argv):
            self.assertEqual(argv[argv.index("--expect") + 1], DIGEST)
            self.assertEqual(argv[argv.index("--min-snapshot") + 1], "1")
            return subprocess.CompletedProcess(argv, status, lines, error)
        self.cli.run_coven = reply
        prefix = "lib 1.0.0: "
        for status, lines, error in [
                (1, prefix + "snapshot 1\n", "does not match its recorded digest\n"),
                (1, prefix + "snapshot 1\n", "not the locked digest\n"),
                (0, prefix + "snapshot 1\n" + prefix + "verified " + other + "\n" + prefix + "unsigned\n", "")]:
            with self.subTest(status=status, error=error):
                for action in ("frozen", "lock"):
                    result = self.decide(action, "consumer/app")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(error.strip() or "was not fetched", result.stderr)
                    self.assertEqual(lock.read_bytes(), before)
        manifest = self.app / "witchy.toml"
        manifest.write_text(manifest.read_text().replace('"1.0.0"', '"1.0.1"'))
        self.refused(self.decide("frozen", "consumer/app"), "does not pin lib 1.0.1")

    def test_producer_snapshot_and_index_refusals_never_replace_a_lock(self):
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        lock = self.app / "witchy.lock"
        before = lock.read_bytes()
        def refuse(reason):
            return subprocess.CompletedProcess([], 1, "", reason)
        self.cli.coven_snapshot = self.wire_snapshot
        with patch.object(self.cli, "run_coven", return_value=refuse("snapshot 2 expired at the signed expiry")):
            for action in ("lock", "frozen"):
                self.refused(self.decide(action, "consumer/app"), "snapshot 2 expired at")
                self.assertEqual(lock.read_bytes(), before)
        self.cli.coven_snapshot = self.verified
        self.cli.coven_fetch = self.wire_fetch
        result = subprocess.CompletedProcess([], 1, "lib 1.0.0: snapshot 1\n", "serves another index 2 than its snapshot 2 names")
        with patch.object(self.cli, "run_coven", return_value=result):
            self.refused(self.decide("lock", "consumer/app"), "serves another index 2 than its snapshot 2 names")
            self.assertEqual(lock.read_bytes(), before)
        self.cli.coven_fetch = self.fetched
        self.snapshot()
        self.assertEqual(self.decide("frozen", "consumer/app").returncode, 0)

    def test_rotation_updates_the_lock_and_rejects_later_snapshots_from_the_old_key(self):
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        successor = key("successor")
        self.rotate(self.operator, successor)
        self.assertEqual(self.decide("frozen", "consumer/app").returncode, 0)
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        lock = self.app / "witchy.lock"
        self.assertIn(f'operator = "{successor}"', lock.read_text())
        self.assertIn(f"{self.operator} -> {successor}, registry test, snapshot 2, retired {self.operator}",
                      self.decide("trust", "consumer/app").stdout)
        self.links = []
        self.served = self.operator, 99
        self.refused(self.decide("frozen", "consumer/app"), f"by an operator chain without {successor}")
        lock.unlink()
        self.refused(self.decide("lock", "consumer/app"), "saw hand the operator role on")
        self.assertFalse(lock.exists())

    def test_snapshot_floor_follows_a_lock_to_a_fresh_machine(self):
        self.served = self.operator, 2
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        self.served = self.operator, 1
        self.refused(self.decide("frozen", "consumer/app", env={"XDG_STATE_HOME": str(self.root / "elsewhere")}),
                     "older than snapshot 2")
        (self.app / "witchy.lock").unlink()
        self.refused(self.decide("lock", "consumer/app"), "older than snapshot 2")
        self.assertFalse((self.app / "witchy.lock").exists())

    def test_first_use_diagnostic_prints_the_exact_declaration_once(self):
        owner, recovery = key("publisher"), key("rescue")
        self.snapshot()
        def signed(*args):
            result = self.fetched(*args)
            result.owner = result.signer = owner
            result.owners = [owner]
            result.index, result.recovery = 1, recovery
            return result
        self.cli.coven_fetch = signed
        locked = self.decide("lock", "consumer/app", first_use=True)
        self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertIn(f'grimoire: [owners]\ngrimoire: lib = {{ owner = "{owner}", recovery = "{recovery}" }}', locked.stderr)
        relocked = self.decide("lock", "consumer/app")
        self.assertEqual(relocked.returncode, 0, relocked.stderr)
        self.assertNotIn("trusted on first use", relocked.stderr)

    def coven_says(self, lines, status=0):
        """Coven's verify-snapshot, misbehaving as told: Grimoire's own checks of its lines are
        what stand. Each argv is kept in `self.argvs`."""
        self.argvs = []
        def reply(argv):
            self.argvs.append(argv)
            return subprocess.CompletedProcess(argv, status, "".join(line + "\n" for line in lines), "")
        self.cli.coven_snapshot = self.wire_snapshot
        return patch.object(self.cli, "run_coven", reply)

    def snapshot_lines(self, sequence, registry="test"):
        return [f"snapshot {sequence}", f"registry {registry}", f"operators {self.operator}", "expires 2099-01-01T00:00:00Z"]

    def test_an_older_snapshot_than_the_locks_is_refused_by_grimoire_itself(self):
        """The lock's snapshot floor reaches a fresh machine: Coven is asked for no older one, and
        Grimoire refuses an older one Coven verified anyway (a rollback)."""
        self.served = self.operator, 2
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        lock = self.app / "witchy.lock"
        before = lock.read_bytes()
        fresh = {"XDG_STATE_HOME": str(self.root / "elsewhere")}
        with self.coven_says(self.snapshot_lines(1)):
            for action in ("frozen", "lock"):
                self.refused(self.decide(action, "consumer/app", env=fresh), "the registry's snapshot is refused")
                self.assertEqual(lock.read_bytes(), before)
        self.assertEqual([argv[argv.index("--min-sequence") + 1] for argv in self.argvs], ["2", "2"])
        with self.coven_says(self.snapshot_lines(2)):
            ran = self.decide("frozen", "consumer/app", env=fresh)
            self.assertEqual(ran.returncode, 0, ran.stderr)

    def test_a_snapshot_coven_refused_is_refused_whatever_it_printed(self):
        self.snapshot()
        self.assertEqual(self.decide("lock", "consumer/app").returncode, 0)
        with self.coven_says(self.snapshot_lines(1), status=1):
            self.refused(self.decide("frozen", "consumer/app"), "the registry's snapshot is refused")
        with self.coven_says(self.snapshot_lines(1)):
            ran = self.decide("frozen", "consumer/app")
            self.assertEqual(ran.returncode, 0, ran.stderr)

    def test_a_snapshot_naming_another_registry_is_refused_by_grimoire_itself(self):
        self.snapshot()
        with self.coven_says(self.snapshot_lines(1, registry="other")):
            self.refused(self.decide("lock", "consumer/app"), "the registry's snapshot is refused")
        self.assertFalse((self.app / "witchy.lock").exists())
        argv, = self.argvs
        self.assertEqual(argv[argv.index("--registry-id") + 1], "test")

    def test_a_fetch_is_bound_to_the_resolves_snapshot(self):
        """Coven is handed this resolve's kept snapshot, and facts of any other snapshot are
        refused, whatever Coven's verdict."""
        self.snapshot()
        self.cli.coven_fetch = self.wire_fetch
        argvs = []
        served = None
        def reply(argv):
            argvs.append(argv)
            into = Path(argv[argv.index("--into") + 1])
            into.mkdir(parents=True, exist_ok=True)
            (into / "package").write_bytes(RECORD)
            (into / "source.wy").write_bytes(b"source")
            prefix = "lib 1.0.0: "
            return subprocess.CompletedProcess(argv, 0, f"{prefix}snapshot {served}\n{prefix}verified {DIGEST}\n{prefix}unsigned\n", "")
        with patch.object(self.cli, "run_coven", reply):
            served = 2
            self.refused(self.decide("lock", "consumer/app"), "was not fetched from the registry")
            self.assertFalse((self.app / "witchy.lock").exists())
            served = 1
            locked = self.decide("lock", "consumer/app")
            self.assertEqual(locked.returncode, 0, locked.stderr)
        self.assertEqual(len(argvs), 2)
        for argv in argvs:
            self.assertEqual(Path(argv[argv.index("--snapshot") + 1]).name, "snapshot")
            self.assertEqual(argv[argv.index("--min-snapshot") + 1], "1")

    def test_a_revocation_seen_here_outlives_the_registry_losing_it(self):
        """A revocation Coven verified is kept by the resolve that refused it, so the version
        stays refused here once the registry serves it as active again."""
        owner, recovery = key("publisher"), key("rescue")
        manifest = self.app / "witchy.toml"
        manifest.write_text(manifest.read_text() + f'[owners]\nlib = {{ owner = "{owner}", recovery = "{recovery}" }}\n')
        self.snapshot()
        def served(revoked):
            def fetch(*args):
                result = self.cli.Fetched() if revoked else self.fetched(*args)
                result.owner, result.owners, result.index, result.recovery = owner, [owner], 1, recovery
                if revoked:
                    result.withdrawn, result.revoked = ("revoked", "CVE-9"), "CVE-9"
                else:
                    result.signer = owner
                return result
            return fetch
        self.cli.coven_fetch = served(True)
        self.refused(self.decide("lock", "consumer/app"), "revoked by its publisher: CVE-9")
        self.cli.coven_fetch = served(False)
        self.refused(self.decide("lock", "consumer/app"), "(CVE-9), as this machine saw")
        self.assertFalse((self.app / "witchy.lock").exists())


if __name__ == "__main__":
    unittest.main()
