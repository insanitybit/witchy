"""The operator's admission rules (`admit`) and a fetch's listing rules (`listing`), called directly
on indexes and real signed rotation statements (ML-DSA-65 through the independent provider, as
the other policy tests sign, where `cryptography` has it; otherwise in the engine). The registry suite runs the same rules end to end
(test_a_stolen_owner_key_cannot_sign_the_recovery_key_out,
test_rotation_hands_a_package_to_a_new_key); these cover the cases one by one, with and without the
operator's record of hand-offs and its copy of the bound rotations.
Run: python3 witchy2/test_index_policy.py"""
import datetime
from pathlib import Path
import tempfile
import unittest

from cli_test_support import fresh_coven, policy_signatures

HERE = Path(__file__).resolve().parent


class IndexPolicy(unittest.TestCase):
    def setUp(self):
        self.enterContext(policy_signatures())
        self.cli = fresh_coven(HERE / "coven")
        self.package = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.until = datetime.date.today() + datetime.timedelta(days=7)
        self.digest = self.cli.digest_of(b"package and source")

    def index(self, owner, sequence, recovery="none", expires=None):
        # `admit` decides on an index already verified (`verify_ahead`), by its fields and digest.
        entries = {"1.0.0": (self.digest, "active", "")}
        return self.cli.Index(owner, sequence, expires or self.until, recovery, entries,
                              f"{owner}/{sequence}/{recovery}".encode())

    def key(self, name):
        # The suite's fixed synthetic seed, with the private file mode a real signer requires.
        source = HERE / "test-keys" / name
        target = self.package / name
        target.write_bytes(source.read_bytes())
        target.chmod(0o600)
        target.with_suffix(".pub").write_bytes(source.with_suffix(".pub").read_bytes())
        return self.cli.signing_key(target)

    def rotation(self, old, new, authority):
        """A rotation statement of lib from `old` to `new`, signed by `authority`."""
        message = self.cli.rotation_message("lib", old.id, new.public, authority.public)
        return self.cli.signed_object(message, self.cli.witchy_sign("rotation", authority, message))

    def admit(self, index, previous, rotations, handed=None, bound=None):
        return self.cli.admit(self.package, "lib", index, previous, rotations, set() if handed is None else handed, bound)

    def test_operator_admits_only_a_continuation_of_the_owner_it_recorded(self):
        alice, bob, carol, rescue = map(self.key, ("alice", "bob", "carol", "rescue"))
        for authority in (None, rescue):
            with self.subTest(recovery=authority and authority.id):
                recovery = authority.id if authority else "none"
                first = self.rotation(alice, bob, authority or alice)
                second = self.rotation(bob, carol, authority or bob)
                handed = set()
                # Handed on by the recovery key, or with none by the owner: admitted, bound, recorded.
                previous = self.cli.entry_of(self.index(alice.id, 1, recovery), None)
                newer = self.index(bob.id, 2, recovery)
                self.assertEqual(self.admit(newer, previous, first, handed), (self.cli.entry_of(newer, first), None, first))
                self.assertEqual(handed, {("lib", alice.id)})
                entry = self.cli.entry_of(newer, first)
                final = self.index(carol.id, 3, recovery)
                self.assertEqual(self.admit(final, entry, first + second, handed), (self.cli.entry_of(final, first + second), None, first + second))
                self.assertEqual(handed, {("lib", alice.id), ("lib", bob.id)})
                current = self.cli.entry_of(final, first + second)
                # A valid prefix is no hand-off from carol back to bob, nor are rotations removed one
                # back to alice: refused by the operator's record of hand-offs; without that record,
                # by the bound rotations (the registry's copy or the operator's); without those, by
                # the bound digest.
                for candidate, served in ((bob, first), (alice, None)):
                    stolen = self.index(candidate.id, 99, recovery)
                    kept, why, binds = self.admit(stolen, current, served, set(handed), first + second)
                    self.assertEqual((kept, binds), (current, None))
                    self.assertIn(f"its index 99 is signed by {candidate.id}, which this operator saw hand lib on", why)
                    kept, why, _ = self.admit(stolen, current, served, None, first + second)
                    self.assertEqual(kept, current)
                    self.assertIn(f"lib's index is signed by {candidate.id}, but its rotations hand it to {carol.id}", why)
                    kept, why, _ = self.admit(stolen, current, served)
                    self.assertEqual(kept, current)
                    self.assertIn("its rotations do not extend the ones snapshot entry 3 bound", why)
                # A different, properly signed branch from the same earlier owner is refused.
                fork = self.rotation(alice, carol, authority or alice)
                kept, why, _ = self.admit(self.index(carol.id, 99, recovery), entry, fork, None, first)
                self.assertEqual(kept, entry)
                self.assertIn(f"but its rotations hand it to {bob.id}", why)
                # The rotations must hand the package on from the owner the entry records: bob, who
                # held lib from its first index, is not handed on by a chain from alice.
                held = self.cli.entry_of(self.index(bob.id, 1, recovery), None)
                kept, why, _ = self.admit(self.index(carol.id, 2, recovery), held, fork)
                self.assertEqual(kept, held)
                self.assertIn(f"its index 2 is signed by {carol.id}, to which {bob.id} did not hand it", why)
                # A statement signed by a key other than the one the package requires.
                wrong = self.rotation(alice, bob, bob if authority else rescue)
                kept, why, _ = self.admit(newer, previous, wrong)
                self.assertEqual(kept, previous)
                self.assertIn("not by the package's recovery key" if authority else "not by the key it hands over", why)
                # The recovery key never changes, and the sequence only grows.
                kept, why, _ = self.admit(self.index(alice.id, 2, bob.id), previous, None)
                self.assertEqual(kept, previous)
                self.assertIn(f"changes the recovery key from {recovery} to {bob.id}", why)
                kept, why, _ = self.admit(self.index(bob.id, 1, recovery), entry, first)
                self.assertEqual(kept, entry)
                self.assertIn("not newer than index 2", why)

    def test_listing_reports_which_index_constraint_failed(self):
        index = self.index("alice", 2, "rescue")
        expired = self.index("alice", 2, "rescue", datetime.date.today() - datetime.timedelta(days=1))
        rows = [
            (index, "alice", 2, "alice", "rescue", self.digest, None),
            (None, "alice", 0, None, None, self.digest, "has no index"),
            (None, None, 2, None, None, self.digest, "index 2 was already seen"),
            (None, None, 0, "alice", None, self.digest, "unsigned, not by the expected alice"),
            (index, "alice", 3, None, None, self.digest, "older than index 3"),
            (expired, "alice", 0, None, None, self.digest, "lib's index expired on"),
            (index, "alice", 0, None, None, "different", "does not list this version with this digest"),
            (index, None, 0, None, None, self.digest, "unsigned, and lib is owned by alice"),
            (index, "bob", 0, None, None, self.digest, "signed by bob, and lib is owned by alice"),
            (index, "alice", 0, "other", None, self.digest, "not by the expected other"),
            (index, "alice", 0, "alice", "other", self.digest, "recovery key is rescue, not the expected other"),
        ]
        for listed, signer, floor, owner, recovery, digest, expected in rows:
            with self.subTest(expected=expected):
                result = self.cli.listing(listed, ["alice"], "lib", "1.0.0", digest, signer, floor, owner, recovery)
                if expected is None:
                    self.assertIsNone(result)
                else:
                    self.assertIn(expected, result)
        self.assertIn("does not list this version", self.cli.listing(index, ["alice"], "lib", "9.9.9",
                                                                    self.digest, None, 0, None, None))
        # An owner key the package was handed from is still one of its owners.
        self.assertIsNone(self.cli.listing(index, ["bob", "alice"], "lib", "1.0.0", self.digest, "bob", 0, "bob", None))

    def test_name_and_version_boundaries(self):
        for name, reason in [("../x", "invalid package name"), ("snapshot", "reserved"), ("operator-rotations", "reserved")]:
            with self.subTest(name=name), self.assertRaisesRegex(self.cli.CovenError, reason):
                self.cli.checked_name(name)
            self.assertEqual(list(self.package.iterdir()), [])
        with self.assertRaisesRegex(self.cli.CovenError, "exact MAJOR.MINOR.PATCH"):
            self.cli.checked_version("1.0")
        self.assertEqual(self.cli.checked_version("1.0.0"), "1.0.0")
        self.assertEqual(self.cli.checked_name("snapshot-reader"), "snapshot-reader")

    def test_unrepresentable_index_is_excluded_without_losing_other_entries(self):
        import contextlib
        import io
        signer = self.key("alice")
        for name, sequence, until in [("lib", 1, self.until), ("evil", 10**19, self.until),
                                      ("future", 3, datetime.date(9999, 12, 31)), ("jump", 2**53 - 1, self.until)]:
            directory = self.package / name
            (directory / "1.0.0").mkdir(parents=True)
            message = self.cli.index_message(name, sequence, until, "none", {}, signer.public)
            raw = self.cli.signed_object(message, self.cli.witchy_sign("index", signer, message))
            (directory / "index").write_bytes(raw)
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            entries, admitted = self.cli.snapshot_entries(self.package, signer.id, None, set())
        self.assertIn("future: not admitted: future: its index expires on 9999-12-31", err.getvalue())
        self.assertIn(f"jump: not admitted: its index {2**53 - 1} is more than {self.cli.MAX_SEQUENCE_STEP} above", err.getvalue())
        self.assertEqual(list(entries), ["lib"])
        self.assertEqual([name for name, _, _ in admitted], ["lib"])
        self.assertIn("evil: not admitted: evil: its index's sequence 10000000000000000000 is above 9007199254740991", err.getvalue())

    def test_review_admits_exactly_the_new_claims(self):
        import contextlib
        import io
        from types import SimpleNamespace
        from unittest.mock import patch
        owner = self.key("alice").id
        first = self.cli.entry_of(self.index(owner, 1), None)
        second = self.cli.entry_of(self.index(owner, 2), None)
        def versions(root, name, entry, path):
            releases = {"1.0.0": (self.digest, "active", "")}
            if entry == second:
                releases["1.0.1"] = releases["1.0.0"]
            return releases
        text = io.StringIO()
        with patch.object(self.cli, "entry_versions", versions), contextlib.redirect_stdout(text):
            claims = {"lib": first, "squat": None}
            with self.assertRaisesRegex(self.cli.CovenError, "give --admit lib --admit squat"):
                self.cli.admission_review([])(self.package, None, claims)
            self.assertIn(f"first claim: lib by {owner}, recovery none, index 1: 1.0.0 active", text.getvalue())
            self.assertIn("first claim: squat, unsigned", text.getvalue())
            with self.assertRaisesRegex(self.cli.CovenError, "other: not a first claim"):
                self.cli.admission_review(["lib", "other"])(self.package, None, claims)
            self.cli.admission_review(claims)(self.package, None, claims)
            self.cli.admission_review([])(self.package, SimpleNamespace(entries=claims), {"lib": second})
            self.assertIn("lib: index 1 -> 2; adds 1.0.1", text.getvalue())

    def test_one_snapshot_retains_both_rotated_packages_when_their_former_signers_reappear(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        import contextlib
        import io
        alice, bob, carol = map(self.key, ("alice", "bob", "carol"))
        prior, candidates = {}, {}
        for name, old, owner, recovery in [("lib", alice, bob, carol.id), ("other", bob, carol, "none")]:
            (self.package / name / "1.0.0").mkdir(parents=True)
            prior[name] = self.cli.entry_of(self.index(owner.id, 2, recovery), None)
            candidates[name] = self.index(old.id, 99, recovery)
        handed = {("lib", alice.id), ("other", bob.id)}
        err = io.StringIO()
        with patch.object(self.cli, "read_index", side_effect=lambda path, name: candidates[name]), contextlib.redirect_stderr(err):
            entries, admitted = self.cli.snapshot_entries(self.package, alice.id, SimpleNamespace(entries=prior, sequence=4), handed)
        self.assertEqual(entries, prior)
        self.assertEqual(admitted, [])
        for name, old in [("lib", alice), ("other", bob)]:
            self.assertIn(f"{name}: kept as in snapshot 4: its index 99 is signed by {old.id}", err.getvalue())

if __name__ == "__main__":
    unittest.main()
