"""A fetched release as Grimoire reads, unpacks and reviews it, called directly: no compiler,
engine or registry runs here. Coven's answer is a stand-in that writes what a registry served.
Run: python3 tests/test_release_review.py"""
import hashlib
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from registry_run import load_cli
from test_release_format import FILES, LIB, NAMES, ONE, TEXT, V2, V2_DIGEST, V2_RECORD, codec, pin, raw


def staged(cli, root, payload, name="lib", version="1.1.0", folder="fetched"):
    """The payload as Coven's fetch leaves it, read and staged as a resolve does."""
    into = root / folder
    into.mkdir()
    record, body = codec.split(payload)
    (into / "package").write_bytes(record)
    (into / "source.wy").write_bytes(body)
    return cli.Staged(name, version, cli.read_release(into, name, version, pin(payload)), into)


class ReleaseReview(unittest.TestCase):
    def setUp(self):
        self.cli = load_cli()
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        self.enterContext(patch.dict(os.environ, {"XDG_STATE_HOME": str(self.root / "state")}))

    def test_grimoire_reads_a_fetched_release_itself_before_anything_is_staged(self):
        cli = self.cli
        into = self.root / "fetched"
        into.mkdir()
        def served(record, body):
            (into / "package").write_bytes(record)
            (into / "source.wy").write_bytes(body)
        def refused(why, digest, name="lib", version="1.1.0"):
            with self.assertRaisesRegex(cli.ProjectError, why):
                cli.read_release(into, name, version, digest)
            self.assertEqual(sorted(path.name for path in into.iterdir()), ["package", "source.wy"])
        body = LIB + NAMES + TEXT
        tampered = body.replace(b"return 64", b"return 65")
        # Not the bytes of the digest Coven reported and the lock pins.
        served(V2_RECORD, tampered)
        refused("is not the one verified", V2_DIGEST)
        # Whatever digest is claimed for them, one file is not the file its record lists.
        refused("file 'names.wy' does not have the digest its record lists", pin(V2_RECORD + tampered))
        function = b"pub fn g() -> U64:\n    return 2\n"
        for files, why in [({"lib.wy": b'import "../x.wy"\n', "../x.wy": function}, "`.` or `..`"),
                           ({"lib.wy": b'import "/tmp/x.wy"\n', "/tmp/x.wy": function}, "relative, not absolute"),
                           ({"lib.wy": b'import "x.wy"\n', "x.wy": function, "y.wy": function}, "which nothing in it imports"),
                           ({"lib.wy": b'import "x.wy"\nimport "y.wy"\n', "x.wy": function}, "which is not a file of the release"),
                           ({"lib.wy": b'import "X.wy"\n', "X.wy": function}, "lower case")]:
            payload = raw(files, order=sorted(files))
            served(*codec.split(payload))
            refused(why, pin(payload))
        served(V2_RECORD, body)
        refused("not the record of other 1.1.0", V2_DIGEST, name="other")
        refused("not the record of lib 9.9.9", V2_DIGEST, version="9.9.9")
        served(V2_RECORD.replace(b"witchy-package 2", b"witchy-package 3"), body)
        refused("use a newer tool", pin(V2_RECORD.replace(b"witchy-package 2", b"witchy-package 3") + body))
        (into / "source.wy").unlink()
        (into / "source.wy").symlink_to(self.root / "elsewhere")
        with self.assertRaisesRegex(cli.ProjectError, "has no `source.wy`"):
            cli.read_release(into, "lib", "1.1.0", V2_DIGEST)
        (into / "source.wy").unlink()
        served(V2_RECORD, body)
        release = cli.read_release(into, "lib", "1.1.0", V2_DIGEST)
        self.assertEqual((release.format, release.requires, dict(release.files)), (2, {"base": "2.0.0"}, FILES))

    def test_a_release_is_staged_for_the_compiler_as_the_files_it_lists(self):
        cli = self.cli
        several = staged(cli, self.root, V2)
        self.assertEqual((several.root, several.entry), (self.root / "fetched/files", self.root / "fetched/files/lib.wy"))
        found = {path.relative_to(several.root).as_posix(): path.read_bytes() for path in several.root.rglob("*") if path.is_file()}
        self.assertEqual(found, FILES)
        self.assertEqual(several.files(), [("@lib", LIB), ("@lib/names.wy", NAMES), ("@lib/util/text.wy", TEXT)])
        # Staged once, into a directory that was not there.
        with self.assertRaisesRegex(cli.ProjectError, "cannot unpack"):
            cli.Staged("lib", "1.1.0", several.release, self.root / "fetched")
        one = staged(cli, self.root, b"witchy-package 1\nname base\nversion 2.0.0\n\n" + ONE, "base", "2.0.0", "one")
        self.assertEqual((one.root, one.entry, one.files()), (self.root / "one/files", self.root / "one/files/source.wy", [("@base", ONE)]))
        self.assertEqual(list(one.root.iterdir()), [one.entry])
        graph = cli.Graph()
        graph["lib"] = ("1.1.0", V2_DIGEST, {"base": "2.0.0"}, self.root / "fetched", "unsigned", 0, "none")
        graph["base"] = ("2.0.0", one.release.digest, {}, self.root / "one", "unsigned", 0, "none")
        graph.staged.update(lib=several, base=one)
        self.assertEqual(cli.package_listing(graph), [f"base 2.0.0: {self.root}/one/files/source.wy ; requires",
                                                      f"lib 1.1.0: {self.root}/fetched/files/lib.wy ; requires base"])
        rewrite = cli.author_names(cli.staged_names(graph))
        self.assertEqual(rewrite(f"{self.root}/fetched/files/util/text.wy:3:1: W0401\n{self.root}/one/files/source.wy:1:1"),
                         "@lib-1.1.0/util/text.wy:3:1: W0401\n@base-2.0.0/source.wy:1:1")
        self.assertEqual(cli.release_files({"lib": several, "base": one}), [
            f"@lib 1.1.0: 3 files in the release {V2_DIGEST}",
            f"  @lib (lib.wy): 106 bytes, {pin(LIB)}", f"  @lib/names.wy: 37 bytes, {pin(NAMES)}", f"  @lib/util/text.wy: 70 bytes, {pin(TEXT)}"])

    def test_single_file_staging_uses_the_verified_bytes(self):
        cli = self.cli
        into = self.root / "fetched"
        into.mkdir()
        record = b"witchy-package 1\nname base\nversion 2.0.0\n\n"
        (into / "package").write_bytes(record)
        transport = into / "source.wy"
        transport.write_bytes(ONE)
        release = cli.read_release(into, "base", "2.0.0", pin(record + ONE))
        transport.write_bytes(b"changed")
        staged = cli.Staged("base", "2.0.0", release, into)
        self.assertEqual(staged.entry.read_bytes(), ONE)
        self.assertEqual(staged.files(), [("@base", ONE)])
        self.assertEqual(transport.read_bytes(), b"changed")

    def test_a_policy_line_is_chosen_as_the_compiler_chooses_it(self):
        rules = self.cli.policy_rules(f"# note\n<standard>/: *\nmain.wy: Console<Write>\n@lib/: Allocator ; no unchecked ; digest {pin(b'a')}\n"
                                      f"@lib/util/: nothing\n@lib/util/text.wy: nothing ; digest {pin(b'b')}  # reviewed\n@other: nothing\n@empty:\n")
        self.assertEqual(rules, [("<standard>/", None), ("main.wy", None), ("@lib/", pin(b"a")), ("@lib/util/", None),
                                 ("@lib/util/text.wy", pin(b"b")), ("@other", None), ("@empty", None)])
        held = {"@lib": "@lib/", "@lib/names.wy": "@lib/", "@lib/util/text.wy": "@lib/util/text.wy", "@lib/util/more.wy": "@lib/util/",
                "@lib/util": "@lib/", "@other": "@other", "@other/x.wy": None, "@library": None, "@lib/util/text.wy.evil": "@lib/util/", "main.wy": "main.wy"}
        for file, prefix in held.items():
            with self.subTest(file=file):
                rule = self.cli.policy_rule(rules, file)
                self.assertEqual(rule and rule[0], prefix)
        # A subdirectory's line is not the package's: it does not cover a file named like it.
        self.assertIsNone(self.cli.policy_rule([("@lib/sub/", None)], "@lib/sub"))
        self.assertEqual(self.cli.tree_listing("@lib/", [("@lib/names.wy", "sha384:n"), ("@lib", "sha384:e")]), b"@lib\0sha384:e\nnames.wy\0sha384:n\n")

    def test_a_review_lists_each_file_added_removed_or_changed_since_it_was_accepted(self):
        cli = self.cli
        changed_names = NAMES.replace(b"return 64", b"let wide = 128\n    return wide")
        extra = b"pub fn more() -> U64:\n    return 3\n"
        new_files = {"lib.wy": b'import "names.wy"\nimport "extra.wy"\n\npub fn longest() -> U64:\n    return names.limit()\n', "names.wy": changed_names, "extra.wy": extra}
        new = staged(cli, self.root, codec.pack("lib", "1.2.0", {}, "lib.wy", new_files), version="1.2.0", folder="new")
        sources = cli.shown_sources()
        kept = lambda data: sources / f"{hashlib.sha384(data).hexdigest()}.wy"
        # File by file: the policy's lines pin what was accepted of each.
        policy = (f"<standard>/: *\nmain.wy: *\n@lib: Allocator ; digest {pin(LIB)}\n@lib/names.wy: nothing ; digest {pin(NAMES)}\n"
                  f"@lib/util/text.wy: nothing ; digest {pin(TEXT)}\n")
        first = cli.release_changes(policy, new)
        self.assertEqual(len(first), 4, first)
        self.assertIn("@lib 1.2.0: differs from the accepted source", first[0])
        self.assertIn(f"read the new source whole, {kept(new_files['lib.wy'])}", first[0])
        self.assertEqual(first[1], f"@lib/extra.wy 1.2.0: a new file (2 lines), in no line of the policy: read it whole, {kept(extra)}")
        self.assertIn("@lib/names.wy 1.2.0: differs from the accepted source", first[2])
        self.assertEqual(first[3], f"@lib/util/text.wy: removed since the accepted source ({pin(TEXT)[:19]}...): 1.2.0 has no such file")
        for data in FILES.values():
            cli.keep_source(data)
        self.assertEqual(cli.release_changes(policy, new), [
            "@lib 1.2.0: 2 lines added, 2 removed in 3 places since the accepted source; in: longest",
            f"  compare: diff -u {kept(LIB)} {kept(new_files['lib.wy'])}",
            first[1],
            "@lib/names.wy 1.2.0: 2 lines added, 1 removed in 1 place since the accepted source; in: limit",
            f"  compare: diff -u {kept(NAMES)} {kept(changed_names)}",
            first[3]])
        # A copy that is not what its name says is not compared with.
        kept(NAMES).write_bytes(b"pub fn limit() -> U64:\n    return 0\n")
        self.assertIn("kept no copy", cli.release_changes(policy, new)[3])
        cli.keep_source(NAMES) if not kept(NAMES).exists() else kept(NAMES).write_bytes(NAMES)
        # Nothing changed: nothing to say. Nothing pinned: nothing was accepted.
        old = staged(cli, self.root, V2, folder="old")
        self.assertEqual(cli.release_changes(policy, old), [])
        self.assertEqual(cli.release_changes("@lib/: Allocator\n", new), [])
        self.assertEqual(cli.release_changes(policy.replace("@lib", "@other"), new), [])

        # One line for the package: its digest is of the files it covers, kept when accepted.
        accepted = [(file, pin(data)) for file, data in old.files()]
        whole = "sha384:" + hashlib.sha384(cli.tree_listing("@lib/", accepted)).hexdigest()
        policy = f"<standard>/: *\nmain.wy: *\n@lib/: Allocator ; no unchecked ; digest {whole}\n"
        self.assertEqual(cli.release_changes(policy, old), [])
        unknown = cli.release_changes(policy, new)
        self.assertIn("@lib/ 1.2.0: differs from the accepted files", unknown[0])
        self.assertIn("kept no list", unknown[0])
        self.assertEqual(unknown[1:], [f"  @lib: {kept(new_files['lib.wy'])}", f"  @lib/extra.wy: {kept(extra)}", f"  @lib/names.wy: {kept(changed_names)}"])
        tree = cli.keep_tree(cli.tree_listing("@lib/", accepted))
        self.assertEqual(tree, sources / f"{whole.removeprefix('sha384:')}.tree")
        self.assertEqual(cli.kept_tree("@lib/", whole), dict(accepted))
        listed = cli.release_changes(policy, new)
        self.assertEqual(listed, [
            "@lib 1.2.0: 2 lines added, 2 removed in 3 places since the accepted source; in: longest",
            f"  compare: diff -u {kept(LIB)} {kept(new_files['lib.wy'])}",
            f"@lib/extra.wy 1.2.0: a new file (2 lines): read it whole, {kept(extra)}",
            "@lib/names.wy 1.2.0: 2 lines added, 1 removed in 1 place since the accepted source; in: limit",
            f"  compare: diff -u {kept(NAMES)} {kept(changed_names)}",
            first[3]])
        # A list that is not what its name says is no list.
        tree.write_bytes(tree.read_bytes().replace(b"names.wy", b"other.wy"))
        self.assertIsNone(cli.kept_tree("@lib/", whole))
        self.assertIn("kept no list", cli.release_changes(policy, new)[0])
        # A package that was one file and is now several: its line pinned that one file.
        single = f"@lib: Allocator ; digest {pin(LIB)}\n"
        grown = cli.release_changes(single, new)
        self.assertEqual([line.split(":")[0] for line in grown if not line.startswith(" ")], ["@lib 1.2.0", "@lib/extra.wy 1.2.0", "@lib/names.wy 1.2.0"])
        self.assertTrue(all("in no line of the policy" in line for line in grown[2:]), grown)
        # Through `source_changes`, as a review prints it, with a checkout's package beside it.
        listing = self.root / "main.packages"
        listing.write_text(f"lib 1.2.0: {new.entry} ; requires\n")
        self.assertEqual(cli.source_changes(single, listing, {"lib": new}), grown)


    def test_every_file_of_a_release_is_pinned_whether_or_not_main_reaches_it(self):
        """Review finding P1: the compiler asks a policy line only of a file with a function
        `main` reaches, so a file of constants alone was pinned by nothing and a version that
        changed only it ran unreviewed. Grimoire asks one of every file of the release."""
        cli = self.cli
        consts = b"pub let FACTOR = 6\n"
        entry = b'import "consts.wy"\n\npub fn answer() -> U64:\n    return 7 * FACTOR\n'
        release = staged(cli, self.root, codec.pack("lib", "1.1.0", {}, "lib.wy", {"lib.wy": entry, "consts.wy": consts}))
        releases = {"lib": release}
        sha = lambda data: "sha384:" + hashlib.sha384(data).hexdigest()
        # What the compiler drafts: a line for the entry only. It "fits" that policy.
        drafted = f"<standard>/: *\nmain.wy: *\n@lib: nothing ; no unchecked ; digest {sha(entry)}\n"
        self.assertEqual(cli.unpinned_files(drafted, releases), ["@lib/consts.wy is in no line of the policy"])
        # The draft Grimoire writes pins it, and then nothing is unpinned.
        whole = cli.pinned_draft(drafted, releases)
        self.assertIn(f"@lib/consts.wy: nothing ; no unchecked ; digest {sha(consts)}\n", whole)
        self.assertEqual(cli.unpinned_files(whole, releases), [])
        self.assertEqual(cli.pinned_draft(whole, releases), whole)
        # The next version changes only the constants: refused by the pin, before any compiler.
        changed = staged(cli, self.root, codec.pack("lib", "1.1.1", {}, "lib.wy", {"lib.wy": entry, "consts.wy": b"pub let FACTOR = 95\n"}),
                         version="1.1.1", folder="next")
        self.assertEqual(cli.unpinned_files(whole, {"lib": changed}),
                         [f"@lib/consts.wy has changed since its line was reviewed: its digest is now {sha(b'pub let FACTOR = 95' + bytes([10]))}"])
        self.assertEqual(cli.unpinned_files(whole, {"lib": changed}, stale=False), [])
        # A line without a digest pins nothing, a file's or a directory's.
        self.assertEqual(cli.unpinned_files("@lib/: *\n", releases),
                         ["@lib/consts.wy is held to a line with no digest (@lib/)", "@lib is held to a line with no digest (@lib/)"])
        self.assertEqual(cli.unpinned_files(f"@lib: * ; digest {sha(entry)}\n@lib/consts.wy: nothing\n", releases),
                         ["@lib/consts.wy is held to a line with no digest (@lib/consts.wy)"])
        # A directory line's digest is of every file it governs, reached or not.
        tree = sha(cli.tree_listing("@lib/", [("@lib", sha(entry)), ("@lib/consts.wy", sha(consts))]))
        self.assertEqual(cli.unpinned_files(f"@lib/: nothing ; digest {tree}\n", releases), [])
        self.assertIn("@lib/ has changed since its line was reviewed", cli.unpinned_files(f"@lib/: nothing ; digest {tree}\n", {"lib": changed})[0])
        # A package of one file whose entry holds no reached function is asked the same.
        one = staged(cli, self.root, codec.pack("one", "1.0.0", {}, "one.wy", {"one.wy": consts}), name="one", version="1.0.0", folder="one")
        self.assertEqual(cli.unpinned_files("main.wy: *\n", {"one": one}), ["@one is in no line of the policy"])
        self.assertEqual(cli.unpinned_files(f"@one: nothing ; digest {sha(consts)}\n", {"one": one}), [])

    def test_a_written_draft_unpins_only_the_projects_own_files_and_the_gate_still_holds_every_release_file(self):
        """`review --write-policy` (and `--replace`) writes `pinned_draft` of the compiler's
        draft. The project's own files lose their digests; a registry package's keep theirs, a
        file `main` does not reach gains one, and the gate a run applies (`unpinned_files`)
        passes that draft and refuses any release file left without a current pin."""
        cli = self.cli
        consts = b"pub let FACTOR = 6\n"
        entry = b'import "consts.wy"\n\npub fn answer() -> U64:\n    return 7 * FACTOR\n'
        releases = {"lib": staged(cli, self.root, codec.pack("lib", "1.1.0", {}, "lib.wy", {"lib.wy": entry, "consts.wy": consts}))}
        sha = lambda data: "sha384:" + hashlib.sha384(data).hexdigest()
        own = sha(b"fn main():\n")
        # What the compiler drafts: a digest on every line, the project's own included.
        compiler = (f"<standard>/wasm-vec.wy: Allocator ; digest {own}\nmain.wy: Console<Write> ; no unchecked ; digest {own}\n"
                    f"helpers/eval.wy: nothing ; no unchecked ; digest {own}\n@lib: nothing ; no unchecked ; digest {sha(entry)}\n")
        written = cli.pinned_draft(compiler, releases)
        lines = written.splitlines()
        self.assertIn("main.wy: Console<Write> ; no unchecked", lines)
        self.assertIn("helpers/eval.wy: nothing ; no unchecked", lines)
        self.assertIn(f"<standard>/wasm-vec.wy: Allocator ; digest {own}", lines)
        self.assertIn(f"@lib: nothing ; no unchecked ; digest {sha(entry)}", lines)
        self.assertIn(f"@lib/consts.wy: nothing ; no unchecked ; digest {sha(consts)}", lines)
        self.assertEqual(cli.unpinned_files(written, releases), [])
        # The gate, on that written policy: a release file whose pin is removed, or whose
        # bytes are another version's, is refused; unpinning is for the project's files alone.
        self.assertEqual(cli.unpinned_files(written.replace(f" ; digest {sha(entry)}", ""), releases), ["@lib is held to a line with no digest (@lib)"])
        self.assertEqual(cli.unpinned_files("\n".join(line for line in lines if not line.startswith("@lib/consts.wy")) + "\n", releases),
                         ["@lib/consts.wy is in no line of the policy"])
        changed = {"lib": staged(cli, self.root, codec.pack("lib", "1.1.1", {}, "lib.wy", {"lib.wy": entry, "consts.wy": b"pub let FACTOR = 95\n"}), version="1.1.1", folder="next")}
        self.assertIn("@lib/consts.wy has changed since its line was reviewed", cli.unpinned_files(written, changed)[0])
        # A package's line is never taken for the project's own, whatever it is called.
        self.assertEqual(cli.own_files_unpinned(f"@lib/main.wy: nothing ; digest {own}\n"), f"@lib/main.wy: nothing ; digest {own}\n")

    def test_reached_files_come_from_the_by_file_listing_and_every_other_release_file_gets_a_line(self):
        """The compiler drafts a line for every file of the bundle now, reached or not, so the
        draft no longer says which files hold reached code. `--by-file` does."""
        cli = self.cli
        listing = ("<standard>/wasm-vec.wy: 0 reachable functions; can use nothing\n  no unchecked memory operations\n"
                   "consts.wy: 0 reachable functions; can use nothing\n  no unchecked memory operations\n"
                   "main.wy: 1 reachable functions; can use Console<Write>\n  no unchecked memory operations\n"
                   "@lib: 3 reachable functions; can use nothing\n@lib/consts.wy: 0 reachable functions; can use nothing\n"
                   "@lib/extra.wy: 1 reachable function; can use Allocator\n")
        reached = cli.reached_files(listing)
        self.assertEqual(reached, {"main.wy", "@lib", "@lib/extra.wy"})
        consts, extra = b"pub let FACTOR = 6\n", b"pub fn more() -> U64:\n    return 1\n"
        entry = b'import "consts.wy"\nimport "extra.wy"\n\npub fn answer() -> U64:\n    return more() * FACTOR\n'
        releases = {"lib": staged(cli, self.root, codec.pack("lib", "1.1.0", {}, "lib.wy", {"lib.wy": entry, "consts.wy": consts, "extra.wy": extra}))}
        zero = "sha384:" + "0" * 96
        accepted = cli.pins_to_accept("main.wy: *\nconsts.wy: nothing\n@lib: nothing\n", releases, reached)
        # The unreached release file gets its line and pin; reached code with no line is still
        # its reviewer's to write; the project's own files are left as their author wrote them.
        self.assertEqual(accepted, f"main.wy: *\nconsts.wy: nothing\n@lib: nothing ; digest {zero}\n@lib/consts.wy: nothing ; no unchecked ; digest {zero}\n")
        # Taken from the draft's lines (every file, now), nothing was appended and the file stayed unpinned.
        every = {"main.wy", "consts.wy", "@lib", "@lib/consts.wy", "@lib/extra.wy"}
        self.assertNotIn("@lib/consts.wy", cli.pins_to_accept("main.wy: *\n@lib: nothing\n", releases, every))
        # Any other file in no line with no reached function gets its `nothing` line at
        # --accept (a library file a new compiler bundles; the project's own constants).
        zeroed = cli.lines_to_accept("main.wy: *\n<standard>/wasm-vec.wy: nothing\n@lib/: *", listing)
        self.assertEqual(zeroed, f"main.wy: *\n<standard>/wasm-vec.wy: nothing\n@lib/: *\nconsts.wy: nothing ; no unchecked\n")
        self.assertEqual(cli.lines_to_accept("main.wy: *\nconsts.wy: nothing\n", listing),
                         f"main.wy: *\nconsts.wy: nothing\n<standard>/wasm-vec.wy: nothing ; no unchecked ; digest {zero}\n@lib/consts.wy: nothing ; no unchecked ; digest {zero}\n")
        self.assertNotIn("@lib/extra.wy", cli.lines_to_accept("main.wy: *\n", listing), "reached code with no line is its author's to hold")
        # The project's own second file: the draft holds it with a line and no digest, which
        # is what the compiler's "in no line of the policy" asks of it.
        own = "sha384:" + "a" * 96
        draft = cli.pinned_draft(f"consts.wy: nothing ; no unchecked ; digest {own}\nmain.wy: Console<Write> ; no unchecked ; digest {own}\n", {})
        self.assertEqual(draft, "consts.wy: nothing ; no unchecked\nmain.wy: Console<Write> ; no unchecked\n")
        self.assertEqual(cli.policy_rules(draft), [("consts.wy", None), ("main.wy", None)])

    def test_accepting_pins_what_no_line_pinned_and_writes_no_line_for_reached_code(self):
        cli = self.cli
        consts, extra = b"pub let FACTOR = 6\n", b"pub fn more() -> U64:\n    return 1\n"
        entry = b'import "consts.wy"\nimport "extra.wy"\n\npub fn answer() -> U64:\n    return more() * FACTOR\n'
        releases = {"lib": staged(cli, self.root, codec.pack("lib", "1.1.0", {}, "lib.wy", {"lib.wy": entry, "consts.wy": consts, "extra.wy": extra}))}
        zero = "sha384:" + "0" * 96
        policy = "# mine\n<standard>/: *\nmain.wy: *\n@lib: nothing ; no unchecked  # the entry\n"
        # `extra.wy` holds a function `main` reaches: its line is its reviewer's to write.
        self.assertEqual(cli.pins_to_accept(policy, releases, {"main.wy", "@lib", "@lib/extra.wy"}),
                         f"# mine\n<standard>/: *\nmain.wy: *\n@lib: nothing ; no unchecked ; digest {zero} # the entry\n"
                         f"@lib/consts.wy: nothing ; no unchecked ; digest {zero}\n")
        # A bare directory line gets a digest and covers every file: nothing is appended.
        self.assertEqual(cli.pins_to_accept("@lib/:\n", releases, set()), f"@lib/: ; digest {zero}\n")
        self.assertEqual(cli.pins_to_accept(f"@lib/: * ; digest {zero}\n", releases, set()), f"@lib/: * ; digest {zero}\n")

    def test_a_new_file_held_to_a_line_with_no_digest_is_listed(self):
        cli = self.cli
        function = b"pub fn g() -> U64:\n    return 2\n"
        entry = b'import "sub/x.wy"\n'
        release = staged(cli, self.root, codec.pack("lib", "1.1.0", {}, "lib.wy", {"lib.wy": entry, "sub/x.wy": function}))
        pin_of = "sha384:" + hashlib.sha384(entry).hexdigest()
        lines = cli.release_changes(f"@lib: * ; digest {pin_of}\n@lib/sub/: *\n", release)
        self.assertEqual(len(lines), 1)
        self.assertRegex(lines[0], r"^@lib/sub/x\.wy 1\.1\.0: a new file \(2 lines\), held to a line with no digest \(@lib/sub/\): read it whole, ")

    def test_only_a_staged_release_is_named_to_the_compiler(self):
        cli = self.cli
        graph = cli.Graph()
        graph["lib"] = ("1.1.0", V2_DIGEST, [], self.root / "nowhere", None, None, None)
        with self.assertRaisesRegex(cli.ProjectError, "lib: no verified release was staged for this build"):
            cli.package_listing(graph)
        graph.staged["lib"] = staged(cli, self.root, V2)
        self.assertEqual(cli.package_listing(graph), [f"lib 1.1.0: {graph.staged['lib'].entry} ; requires"])


    def test_a_listed_checkout_package_is_held_by_a_line_whatever_main_reaches(self):
        cli = self.cli
        listing = self.root / "main.packages"
        listing.write_text("# local\ndep 0.1: ../dep/dep.wy ; requires leaf\nleaf: ../leaf/leaf.wy\n")
        self.assertEqual(cli.unheld_checkouts("<standard>/: *\nmain.wy: *\n", listing),
                         ["@dep is in no line of the policy", "@leaf is in no line of the policy"])
        self.assertEqual(cli.unheld_checkouts("@dep: nothing\n@leaf/: *\n", listing), [])
        self.assertEqual(cli.unheld_checkouts("@depx: *\n@leaf/sub/: *\n", listing),
                         ["@dep is in no line of the policy", "@leaf is in no line of the policy"])
        self.assertEqual(cli.unheld_checkouts("main.wy: *\n", self.root / "none.packages"), [])


if __name__ == "__main__":
    unittest.main()
