"""Resolver contracts starting at Coven's Fetched boundary. registry_run covers the wire and signatures."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from registry_run import load_cli


def key(label):
    return "witchy-2:" + hashlib.sha384(label.encode()).hexdigest()


ALICE, BOB, CAROL, RESCUE, OTHER = map(key, ("alice", "bob", "carol", "rescue", "other"))
NAME, VERSION = "lib", "1.0.0"
RECORD = b"witchy-package 1\nname lib\nversion 1.0.0\n\n"
DIGEST = "sha384:" + hashlib.sha384(RECORD + b"source").hexdigest()


class TrustPolicy(unittest.TestCase):
    def setUp(self):
        self.cli = load_cli()
        work = tempfile.TemporaryDirectory()
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)
        self.enterContext(patch.dict(os.environ, XDG_STATE_HOME=str(self.root / "state")))
        (self.root / "registry").mkdir()
        self.manifest = self.root / "witchy.toml"
        self.manifest.write_text('[registry]\npath = "registry"\n[dependencies]\nlib = "1.0.0"\n')
        self.registry, _ = self.cli.read_dependencies(self.manifest)
        self.registry.trust_first_use = True

    def decide(self, owners, index, *, floor, requested_owner, requested_recovery,
               recovery=RESCUE, declared=None, pinned=None, withdrawn=None, frozen=False):
        """Supply index facts to the consumer. Check what it actually asks the producer to pin."""
        self.registry.owners = {NAME: declared} if declared else {}
        pins = {NAME: (VERSION, DIGEST, [], pinned[0], pinned[1], pinned[2])} if pinned else {}
        facts = self.cli.Fetched()
        facts.owners, facts.owner, facts.signer = list(owners), owners[-1], owners[0]
        facts.index, facts.recovery, facts.digest = index, recovery, DIGEST
        facts.withdrawn = withdrawn
        facts.ok = withdrawn is None
        if withdrawn:
            facts.revoked = withdrawn[1]

        def fetch(registry, name, version, into, digest, owner, min_index, anchor, snapshot):
            self.assertEqual((registry, name, version, digest, owner, min_index, anchor),
                             (self.registry, NAME, VERSION, DIGEST if pinned else None,
                              requested_owner, floor, requested_recovery))
            into.mkdir(parents=True)
            (into / "package").write_bytes(RECORD)
            (into / "source.wy").write_bytes(b"source")
            return facts

        store = self.cli.TrustStore(self.registry)
        try:
            with tempfile.TemporaryDirectory(dir=self.root) as temp, patch.object(self.cli, "coven_fetch", fetch):
                graph, _ = self.cli.resolve_with(self.registry, {NAME: VERSION}, pins, temp, frozen, store, None)
                self.assertEqual(graph[NAME][:3], (VERSION, DIGEST, {}))
                return graph[NAME][4:]
        finally:
            try:
                store.save()
            finally:
                store.close()

    def trust(self, forget=None):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.cli.trust_project(self.manifest, forget=forget)
        return out.getvalue()

    def state(self):
        store = self.cli.TrustStore(self.registry)
        try:
            return json.loads(store.path.read_text())
        finally:
            store.close()

    def test_retirement_survives_a_stale_lock_and_forget(self):
        self.decide([ALICE, BOB], 2, floor=0, requested_owner=None, requested_recovery=None)
        for forgot in (False, True):
            with self.subTest(forgot=forgot):
                if forgot:
                    self.assertIn("its retired keys", self.trust(forget=NAME))
                before = self.state()
                for _ in range(2):
                    with self.assertRaisesRegex(self.cli.ProjectError, "rotated-away key is never its owner again"):
                        self.decide([ALICE], 999999, pinned=(ALICE, 1, RESCUE), frozen=True,
                                    floor=2, requested_owner=ALICE, requested_recovery=RESCUE)
                self.assertEqual(self.state(), before)
                self.assertNotIn("conflict", self.trust())
                self.assertNotIn("999999", self.trust())

    def test_hidden_and_truncated_rotations_preserve_the_longest_chain(self):
        self.decide([BOB], 3, floor=0, requested_owner=None, requested_recovery=None)
        self.decide([ALICE, BOB, CAROL], 4, floor=3, requested_owner=BOB, requested_recovery=RESCUE)
        self.decide([BOB, CAROL], 5, floor=4, requested_owner=CAROL, requested_recovery=RESCUE)
        self.assertEqual(self.state()["packages"][NAME]["owners"], [ALICE, BOB, CAROL])
        self.assertEqual(self.state()["packages"][NAME]["retired"], [ALICE, BOB])
        self.assertIn(f"retired {ALICE} {BOB}", self.trust())
        for forgot in (False, True):
            if forgot:
                self.trust(forget=NAME)
            for chain in ([ALICE], [ALICE, BOB]):
                with self.subTest(forgot=forgot, chain=chain):
                    before = self.state()
                    with self.assertRaisesRegex(self.cli.ProjectError, "rotated-away key"):
                        self.decide(chain, 999999, pinned=(ALICE, 1, RESCUE), frozen=True,
                                    requested_owner=ALICE, requested_recovery=RESCUE, floor=5)
                    self.assertEqual(self.state(), before)

    def test_a_reset_under_the_same_recovery_key_keeps_prior_retirements(self):
        self.decide([BOB, CAROL], 4, floor=0, requested_owner=None, requested_recovery=None)
        with self.assertRaisesRegex(self.cli.ProjectError, "rotated-away key"):
            self.decide([ALICE, BOB], 999999, pinned=(ALICE, 1, RESCUE), frozen=True,
                        floor=4, requested_owner=ALICE, requested_recovery=RESCUE)
        self.assertIn(CAROL, self.trust())
        self.assertNotIn("999999", self.trust())

    def test_a_first_use_reset_by_a_contradicting_anchor_keeps_its_retirements(self):
        # The stated recovery key is the first use's: what it saw handed on stays retired
        # through the reset (e7ebfaf; the accept half of the removed end-to-end test).
        self.decide([ALICE, BOB], 2, floor=0, requested_owner=None, requested_recovery=None)
        self.decide([CAROL], 3, pinned=(CAROL, 1, RESCUE), floor=2, requested_owner=CAROL, requested_recovery=RESCUE)
        self.assertEqual(self.state()["packages"][NAME]["retired"], [ALICE])
        with self.assertRaisesRegex(self.cli.ProjectError, "rotated-away key is never its owner again"):
            self.decide([ALICE], 999999, pinned=(ALICE, 1, RESCUE), frozen=True,
                        floor=3, requested_owner=ALICE, requested_recovery=RESCUE)

    def test_a_first_use_under_another_recovery_key_does_not_outlive_an_anchored_resolve(self):
        # A first use took a thief's recovery key; the reviewed lock's chain holds the first use's
        # owner and states the real key: it replaces the record (1d1bb93), never extends it.
        self.decide([ALICE], 1, recovery=OTHER, floor=0, requested_owner=None, requested_recovery=None)
        self.decide([ALICE, BOB], 2, pinned=(ALICE, 1, RESCUE), floor=1,
                    requested_owner=ALICE, requested_recovery=RESCUE)
        shown = self.trust()
        self.assertIn(f"recovery {RESCUE}", shown)
        self.assertNotIn(OTHER, shown)
        self.assertEqual(self.decide([ALICE, BOB], 2, pinned=(BOB, 2, RESCUE), frozen=True, floor=2,
                                     requested_owner=BOB, requested_recovery=RESCUE), (BOB, 2, RESCUE))

    def test_only_a_stated_recovery_key_can_replace_a_first_use(self):
        self.decide([ALICE, CAROL], 2, recovery=OTHER, floor=0, requested_owner=None, requested_recovery=None)
        self.assertIn("(first use)", self.trust())
        self.assertIn(f"recovery {OTHER}", self.trust())
        with self.assertRaisesRegex(self.cli.ProjectError, "this machine recorded"):
            self.decide([ALICE, BOB], 3, declared=(ALICE, None), floor=2,
                        requested_owner=ALICE, requested_recovery=None)
        self.assertIn("(first use)", self.trust())
        self.decide([ALICE, BOB], 3, pinned=(ALICE, 1, RESCUE), floor=2,
                    requested_owner=ALICE, requested_recovery=RESCUE)
        self.assertIn(f"recovery {RESCUE}", self.trust())
        self.assertNotIn(OTHER, self.trust())

    def test_a_served_recovery_key_cannot_relabel_forgotten_retirements(self):
        self.decide([ALICE, BOB], 2, floor=0, requested_owner=None, requested_recovery=None)
        self.trust(forget=NAME)
        with self.assertRaisesRegex(self.cli.ProjectError, "this machine recorded"):
            self.decide([BOB], 500000, declared=(BOB, None), recovery="none", floor=2,
                        requested_owner=BOB, requested_recovery=None)
        self.assertIn(f"recovery {RESCUE}", self.trust())
        self.assertIn(f"retired {ALICE}", self.trust())

    def test_confirmed_then_forgotten_first_use_can_be_reanchored(self):
        self.decide([ALICE, CAROL], 2, recovery=OTHER, floor=0, requested_owner=None, requested_recovery=None)
        self.decide([ALICE, CAROL], 2, recovery=OTHER, pinned=(CAROL, 2, OTHER), frozen=True,
                    floor=2, requested_owner=CAROL, requested_recovery=OTHER)
        self.assertNotIn("(first use)", self.trust())
        with self.assertRaisesRegex(self.cli.ProjectError, "grimoire trust --forget"):
            self.decide([ALICE], 2, pinned=(ALICE, 1, RESCUE), floor=2,
                        requested_owner=ALICE, requested_recovery=RESCUE)
        self.trust(forget=NAME)
        self.decide([ALICE], 2, pinned=(ALICE, 1, RESCUE), floor=2,
                    requested_owner=ALICE, requested_recovery=RESCUE)
        shown = self.trust()
        self.assertIn(f"recovery {RESCUE}", shown)
        self.assertNotIn(f"retired {ALICE}", shown)
        self.assertNotIn("(first use)", shown)

    def test_only_an_admissible_index_can_raise_the_floor_or_record_revocation(self):
        self.decide([ALICE, BOB], 2, declared=(ALICE, RESCUE), floor=0,
                    requested_owner=ALICE, requested_recovery=RESCUE)
        before = self.state()
        with self.assertRaisesRegex(self.cli.ProjectError, "rotated-away key"):
            self.decide([ALICE], 999999, withdrawn=("revoked", "wrong"), pinned=(ALICE, 1, RESCUE),
                        floor=2, requested_owner=ALICE, requested_recovery=RESCUE)
        self.assertEqual(self.state(), before)
        with self.assertRaisesRegex(self.cli.ProjectError, "revoked by its publisher: CVE-1"):
            self.decide([ALICE, BOB], 4, withdrawn=("revoked", "CVE-1"), pinned=(ALICE, 1, RESCUE),
                        floor=2, requested_owner=ALICE, requested_recovery=RESCUE)
        self.assertEqual(self.state()["packages"][NAME]["index"], 4)
        self.assertEqual(self.state()["revoked"], {"lib 1.0.0": "CVE-1"})
        # A different version was never recorded as revoked. Its request must
        # still carry the package-wide floor learnt from the refused version.
        store = self.cli.TrustStore(self.registry)
        try:
            with patch.object(self.cli, "coven_fetch", side_effect=self.cli.ProjectError("producer refused")) as fetch:
                with self.assertRaisesRegex(self.cli.ProjectError, "producer refused"):
                    self.cli.resolve_with(self.registry, {NAME: "2.0.0"}, {}, self.root, False, store, None)
                self.assertEqual(fetch.call_args.args[1:3], (NAME, "2.0.0"))
                self.assertEqual(fetch.call_args.args[6], 4)
        finally:
            store.close()
        with self.assertRaisesRegex(self.cli.ProjectError, "stays refused here"):
            self.decide([ALICE, BOB], 5, floor=4, requested_owner=BOB, requested_recovery=RESCUE)

    def test_bare_declaration_uses_a_trusted_recovery_and_respects_the_lock(self):
        self.decide([ALICE, BOB], 2, declared=(ALICE, RESCUE), floor=0,
                    requested_owner=ALICE, requested_recovery=RESCUE)
        self.assertEqual(self.decide([ALICE, BOB], 3, declared=(ALICE, None), floor=2,
                                     requested_owner=ALICE, requested_recovery=RESCUE), (BOB, 3, RESCUE))
        self.assertEqual(self.decide([ALICE, BOB], 3, floor=3, requested_owner=BOB,
                                     requested_recovery=RESCUE), (BOB, 3, RESCUE))
        with self.assertRaisesRegex(self.cli.ProjectError, f"without {BOB}, which witchy.lock pins"):
            self.decide([ALICE, CAROL], 4, declared=(ALICE, None), pinned=(BOB, 3, RESCUE),
                        floor=3, requested_owner=ALICE, requested_recovery=RESCUE)



    def test_only_this_projects_pins_or_explicit_first_use_authorize_an_owner(self):
        self.registry.first_used = {}
        self.registry.trust_first_use = False
        with self.assertRaises(self.cli.ProjectError) as error:
            self.decide([ALICE], 1, floor=0, requested_owner=None, requested_recovery=None)
        self.assertIn(f'{NAME} = {{ owner = "{ALICE}", recovery = "{RESCUE}" }}', str(error.exception))
        self.assertFalse(self.registry.first_used)
        self.assertNotIn(ALICE, self.trust())
        self.registry.trust_first_use = True
        self.decide([ALICE], 1, floor=0, requested_owner=None, requested_recovery=None)
        self.assertEqual(self.registry.first_used, {NAME: (ALICE, RESCUE)})
        self.registry.first_used.clear()
        self.registry.trust_first_use = False
        for authority in ({"pinned": (ALICE, 1, RESCUE)}, {"declared": (ALICE, RESCUE)}):
            self.decide([ALICE], 1, floor=1, requested_owner=ALICE, requested_recovery=RESCUE, **authority)
            self.assertFalse(self.registry.first_used)
            with self.assertRaisesRegex(self.cli.ProjectError, "neither this project's .* nor its lock anchors"):
                self.decide([ALICE], 1, floor=1, requested_owner=ALICE, requested_recovery=RESCUE)

    def test_a_declared_owner_and_reviewed_pin_must_both_be_in_the_chain(self):
        for declared, pinned, missing in [((BOB, RESCUE), None, BOB), (None, (BOB, 1, RESCUE), BOB),
                                          ((ALICE, RESCUE), (BOB, 1, RESCUE), BOB)]:
            with self.subTest(missing=missing), self.assertRaisesRegex(self.cli.ProjectError, f"without {missing}"):
                self.decide([ALICE], 20, declared=declared, pinned=pinned, floor=1 if pinned else 0,
                            requested_owner=declared[0] if declared else BOB, requested_recovery=RESCUE)
            self.assertNotIn("index 20", self.trust())
        self.assertEqual(self.decide([ALICE, BOB], 2, declared=(ALICE, RESCUE), floor=0,
                                     requested_owner=ALICE, requested_recovery=RESCUE), (BOB, 2, RESCUE))

    def test_first_use_fork_and_forget_respect_reviewed_successors(self):
        self.decide([ALICE, BOB], 2, floor=0, recovery="none", requested_owner=None, requested_recovery=None)
        for frozen in (True, False):
            with self.assertRaisesRegex(self.cli.ProjectError, "does not continue the one this machine verified"):
                self.decide([ALICE, CAROL], 50, recovery="none", pinned=(ALICE, 1, "none"), frozen=frozen,
                            floor=2, requested_owner=ALICE, requested_recovery="none")
        self.assertNotIn("conflict", self.trust())
        self.assertIn("its retired keys", self.trust(forget=NAME))
        with self.assertRaisesRegex(self.cli.ProjectError, "forgot its owners"):
            self.decide([ALICE, CAROL], 3, recovery="none", floor=2, requested_owner=None, requested_recovery=None)
        self.assertEqual(self.decide([ALICE, CAROL], 3, recovery="none", pinned=(ALICE, 1, "none"), frozen=True,
                                     floor=2, requested_owner=ALICE, requested_recovery="none"), (CAROL, 3, "none"))

    def test_declared_owner_and_signed_only_requirements(self):
        self.registry.owners_required = True
        with self.assertRaisesRegex(self.cli.ProjectError, "has no owner declared in"):
            self.decide([ALICE], 1, floor=0, requested_owner=None, requested_recovery=None)
        self.registry.signed = True
        with self.assertRaisesRegex(self.cli.ProjectError, "asks for signed packages"):
            self.cli.check_signer(self.registry, NAME, VERSION, "unsigned", None, False)

    def test_yank_and_revoke_for_locked_and_new_versions(self):
        out = io.StringIO()
        with contextlib.redirect_stderr(out):
            self.cli.check_withdrawn(NAME, VERSION, ("yanked", "use 1.0.1"), kept=True)
        self.assertIn("yanked by its publisher: use 1.0.1", out.getvalue())
        with self.assertRaisesRegex(self.cli.ProjectError, "yanked by its publisher: use 1.0.1"):
            self.cli.check_withdrawn(NAME, VERSION, ("yanked", "use 1.0.1"), kept=False)
        for kept in (False, True):
            with self.assertRaisesRegex(self.cli.ProjectError, "revoked by its publisher: CVE-7"):
                self.cli.check_withdrawn(NAME, VERSION, ("revoked", "CVE-7"), kept)

    def test_frozen_unsigned_lock_requires_an_explicit_relock_for_a_new_signer(self):
        pins = ("1.0.0", DIGEST, [], "unsigned", 0, "none")
        with self.assertRaisesRegex(self.cli.ProjectError, "run .grimoire lock") as error:
            self.cli.check_signer(self.registry, NAME, VERSION, ALICE, pins, True)
        self.assertNotIn("--trust-first-use", str(error.exception))
        with self.assertRaisesRegex(self.cli.ProjectError, "is unsigned"):
            self.cli.check_signer(self.registry, NAME, VERSION, "unsigned", (*pins[:3], ALICE, 1, RESCUE), False)
        self.registry.first_used = {}
        self.decide([ALICE], 1, floor=0, requested_owner=None, requested_recovery=None,
                    pinned=("unsigned", 0, "none"))
        self.assertEqual(self.registry.first_used, {NAME: (ALICE, RESCUE)})

    def test_frozen_owner_and_recovery_pins_hold_after_an_unrelated_first_use(self):
        self.decide([ALICE, CAROL], 5, recovery=OTHER, floor=0, requested_owner=None, requested_recovery=None)
        before = self.state()
        with self.assertRaisesRegex(self.cli.ProjectError, f"without {BOB}"):
            self.decide([CAROL], 6, pinned=(BOB, 1, RESCUE), frozen=True,
                        floor=5, requested_owner=BOB, requested_recovery=RESCUE)
        self.assertEqual(self.state(), before)
        self.assertEqual(self.decide([BOB], 6, pinned=(BOB, 1, RESCUE), frozen=True,
                                     floor=5, requested_owner=BOB, requested_recovery=RESCUE), (BOB, 6, RESCUE))
        self.assertEqual(self.state()["packages"][NAME]["owners"], [BOB])

    def test_an_unrelated_writers_chain_cannot_freeze_a_confirmed_owner(self):
        self.decide([ALICE, BOB], 2, declared=(ALICE, RESCUE), floor=0,
                    requested_owner=ALICE, requested_recovery=RESCUE)
        before = self.state()
        for chain, recovery in [([CAROL], RESCUE), ([ALICE], OTHER)]:
            with self.assertRaises(self.cli.ProjectError):
                self.decide(chain, 999999, recovery=recovery, pinned=(BOB, 2, RESCUE), frozen=True,
                            floor=2, requested_owner=BOB, requested_recovery=RESCUE)
            self.assertEqual(self.state(), before)
        self.assertEqual(self.decide([ALICE, BOB], 2, declared=(BOB, None), pinned=(ALICE, 1, RESCUE),
                                     frozen=True, floor=2, requested_owner=BOB, requested_recovery=RESCUE),
                         (BOB, 2, RESCUE))

    def test_a_reviewed_successor_displaces_a_lockless_fork_without_recovery(self):
        self.decide([ALICE, CAROL], 2, recovery="none", floor=0, requested_owner=None, requested_recovery=None)
        actual = self.decide([ALICE, BOB], 2, pinned=(BOB, 2, "none"), frozen=True, recovery="none",
                             floor=2, requested_owner=BOB, requested_recovery="none")
        self.assertEqual(actual, (BOB, 2, "none"))
        with self.assertRaises(self.cli.ProjectError):
            self.decide([ALICE, CAROL], 3, recovery="none", floor=2, requested_owner=BOB, requested_recovery="none")
        self.assertEqual(self.state()["packages"][NAME]["owners"], [ALICE, BOB])

    def test_an_index_under_another_recovery_key_than_declared_is_refused_by_grimoire_itself(self):
        # Coven is asked for the declared key; a Coven that verifies another anyway is not believed.
        with self.assertRaisesRegex(self.cli.ProjectError, f"its recovery key is {OTHER}, and witchy.toml declares {RESCUE}"):
            self.decide([ALICE], 1, recovery=OTHER, declared=(ALICE, RESCUE), floor=0,
                        requested_owner=ALICE, requested_recovery=RESCUE)
        self.assertNotIn(NAME, self.state()["packages"])

    def test_a_bare_declaration_does_not_promote_a_first_use(self):
        # 349b07e: only an anchor stating the recovery key makes a first use trusted, so a
        # project's stated key can still replace a first use a bare declaration matched.
        self.decide([ALICE], 1, recovery=OTHER, floor=0, requested_owner=None, requested_recovery=None)
        self.decide([ALICE], 1, recovery=OTHER, declared=(ALICE, None), floor=1,
                    requested_owner=ALICE, requested_recovery=None)
        self.assertTrue(self.state()["packages"][NAME]["provisional"])
        self.assertIn("(first use)", self.trust())
        self.assertEqual(self.decide([ALICE], 1, pinned=(ALICE, 1, RESCUE), floor=1,
                                     requested_owner=ALICE, requested_recovery=RESCUE), (ALICE, 1, RESCUE))
        self.assertEqual(self.state()["packages"][NAME]["recovery"], RESCUE)

if __name__ == "__main__":
    unittest.main()
