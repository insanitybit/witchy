"""Direct checks of a run's directory rights and limits: what the document may state, what a
writable root may not hold, and which limits are passed and shown. No compiler, no engine."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from registry_run import load_cli


class _Listing(list):
    """What `os.scandir` returns, from entries already read."""
    def __enter__(self):
        return self
    def __exit__(self, *exc):
        return False


class RunGrants(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cli = load_cli()

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.app = self.root / "app"
        (self.app / "data/notes").mkdir(parents=True)
        (self.app / "lib").mkdir()
        self.manifest = self.app / "witchy.toml"
        self.manifest.write_text('[run]\nentry = "main.wy"\n')
        self.entry = self.app / "main.wy"
        self.entry.write_text("fn main():\n    return\n")
        (self.app / "lib/helper.wy").write_text("pub fn one() -> U64:\n    return 1\n")
        self.document = self.app / "main.grants.toml"
        self.document.write_text('[dirs]\ndata = { root = "data", rights = ["Read", "Write"] }\n')
        # A machine of its own: state, cache, proofs and tools all under this test's root.
        self.places = {"XDG_STATE_HOME": self.root / "home/state", "XDG_CACHE_HOME": self.root / "home/cache",
                       "WITCHY_PROOF_CACHE": self.root / "home/proofs"}
        self.tools = {"the compiler": self.root / "tools/bin/witchyc", "the engine": self.root / "tools/bin/witchy-wasmtime-host"}
        (self.root / "tools/bin").mkdir(parents=True)
        (self.root / "home/proofs").mkdir(parents=True)
        (self.root / "home/state/grimoire/trust").mkdir(parents=True)

    def writable(self, root):
        with patch.dict(os.environ, {name: str(path) for name, path in self.places.items()}):
            protected, stores = self.cli.protected_paths(self.manifest, self.entry, self.document, self.tools)
            self.cli.check_writable_root(str(root), protected, stores)

    def test_a_document_states_read_write_or_both_and_nothing_else(self):
        read = self.cli.read_directory_grants
        self.assertEqual(read(self.document, {"dirs": {"data": {"root": "data", "rights": ["Read", "Write"]}}}), {"data": ("data", "read,write")})
        self.assertEqual(read(self.document, {"dirs": {"data": {"root": "data", "rights": ["Write", "Read"]}}}), {"data": ("data", "read,write")})
        self.assertEqual(read(self.document, {"dirs": {"data": {"root": "data", "rights": ["Write"]}}}), {"data": ("data", "write")})
        self.assertEqual(read(self.document, {"dirs": {"data": {"root": "data"}}}), {"data": ("data", None)})
        self.assertEqual(read(self.document, {}), {})
        # The refusal names the document's entry, in the document's words: never an engine flag.
        for rights, message in ((["Read", "Execute"], r"main\.grants\.toml: \[dirs\] data: unsupported right 'Execute'"),
                                (["read"], r"\[dirs\] data: unsupported right 'read'"),
                                (["Write", "Write"], r"\[dirs\] data: a right is stated twice"),
                                ([], r"\[dirs\] data: rights is a list"), ("Write", r"\[dirs\] data: rights is a list")):
            with self.subTest(rights=rights), self.assertRaisesRegex(self.cli.ProjectError, message) as refused:
                read(self.document, {"dirs": {"data": {"root": "data", "rights": rights}}})
            self.assertNotIn("--dir", str(refused.exception))
        with self.assertRaisesRegex(self.cli.ProjectError, r"\[dirs\] data: a directory is"):
            read(self.document, {"dirs": {"data": "data"}})

    def test_sqlite_authority_matches_the_document(self):
        self.manifest.write_text('[run]\nentry = "main.wy"\n[grants]\ndocument = "main.grants.toml"\n')
        self.document.write_text('[sqlite]\ndb = {file = "data/items.db", read = ["items"], reply_bytes = 32}\n')
        db = self.app / "data/items.db"
        db.touch()
        declared = [f"--sqlite={db}", "--sqlite-read=items", "--sqlite-reply-bytes=32"]

        @contextlib.contextmanager
        def review(*args, **kwargs):
            yield subprocess.CompletedProcess(["witchyc"], 0, "\n".join(proposed), ""), build

        def build(module):
            module.write_bytes(b"built")

        compiler = self.cli.compiler_client()
        output = self.root / "built.wasm"
        with patch.object(compiler, "reviewed_build", review), patch.object(self.cli, "tool", return_value=str(self.tools["the compiler"])):
            for proposed in (declared[:-1], declared + ["--sqlite-delete=items"], declared + declared[:1],
                             [f"--sqlite={self.app / 'data/other.db'}", *declared[1:]]):
                with self.subTest(proposed=proposed), self.assertRaisesRegex(self.cli.ProjectError, "SQLite grant disagrees"):
                    self.cli.run_project(self.manifest, build_only=True, output=output)
                self.assertFalse(output.exists())
            proposed = declared[::-1]
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as report:
                self.cli.run_project(self.manifest, build_only=True, output=output)
            self.assertEqual(output.read_bytes(), b"built")
            self.assertIn("grant: SQLite tables: read items", report.getvalue())
            self.assertIn("limit: SQLite reply: 32 bytes", report.getvalue())

    def test_sqlite_files_and_sidecars_stay_inside_the_project(self):
        db = self.app / "data/items.db"
        db.touch()
        protected = {"the executable's grant document": self.document}
        self.cli.check_sqlite_files(str(db), self.manifest, protected, {})
        with self.assertRaisesRegex(self.cli.ProjectError, "must be inside the project"):
            self.cli.check_sqlite_files(str(self.root / "outside.db"), self.manifest, protected, {})
        sidecar = db.with_suffix(".db-wal")
        sidecar.symlink_to(self.document)
        with self.assertRaisesRegex(self.cli.ProjectError, "without links or aliases"):
            self.cli.check_sqlite_files(str(db), self.manifest, protected, {})
        sidecar.unlink()
        sidecar.hardlink_to(self.document)
        with self.assertRaisesRegex(self.cli.ProjectError, "is the executable's grant document"):
            self.cli.check_sqlite_files(str(db), self.manifest, protected, {})

    def test_a_writable_root_holds_nothing_that_decides_the_next_runs_authority(self):
        # Its own data directory, and one below it: granted.
        self.writable(self.app / "data")
        self.writable(self.app / "data/notes")
        refusals = [
            (self.app, "holds the project's manifest"),
            (self.root, "holds the project's manifest"),
            (self.app / "lib", "holds the project's source lib/helper.wy"),
            (self.root / "home/state", "holds Grimoire's state"),
            (self.root / "home/state/grimoire", "holds Grimoire's state"),
            (self.root / "home/state/grimoire/trust", "is inside Grimoire's state"),
            (self.root / "home", "holds Grimoire's state"),
            (self.root / "home/proofs", "holds the proof cache"),
            (self.root / "tools", "holds the compiler"),
            (self.root / "tools/bin", "holds the compiler"),
            (Path(self.cli.__file__).resolve().parent, "holds Grimoire"),
        ]
        for root, message in refusals:
            with self.subTest(root=root), self.assertRaisesRegex(self.cli.ProjectError, message):
                self.writable(root)
        # What does not exist yet is protected by where it would be made: the lock is not
        # written, Coven has kept nothing, and neither directory is granted for it.
        self.assertFalse((self.app / "witchy.lock").exists() or (self.root / "home/state/coven").exists())
        (self.root / "home/cache").mkdir()
        with self.assertRaisesRegex(self.cli.ProjectError, "holds Coven's module cache"):
            self.writable(self.root / "home/cache")
        # By identity, not spelling: the project reached through a link, a policy that is a link
        # into the data directory, and a source hard-linked there under another name's directory.
        (self.root / "alias").symlink_to(self.app)
        with self.assertRaisesRegex(self.cli.ProjectError, "holds the project's manifest"):
            self.writable(self.root / "alias")
        (self.app / "data/kept.policy").write_text("main.wy: *\n")
        (self.app / "main.policy").symlink_to("data/kept.policy")
        with self.assertRaisesRegex(self.cli.ProjectError, "holds the executable's policy"):
            self.writable(self.app / "data")
        (self.app / "main.policy").unlink()
        # A checkout package's source, listed beside the entry.
        (self.root / "pkg").mkdir()
        (self.root / "pkg/lib.wy").write_text("pub fn two() -> U64:\n    return 2\n")
        (self.app / "main.packages").write_text("pkg 1.0: ../pkg/lib.wy\n")
        with self.assertRaisesRegex(self.cli.ProjectError, "holds the package pkg's source"):
            self.writable(self.root / "pkg")
        # A source file inside the data directory itself is the project's source.
        (self.app / "data/generated.wy").write_text("pub fn three() -> U64:\n    return 3\n")
        with self.assertRaisesRegex(self.cli.ProjectError, "holds the project's source data/generated.wy"):
            self.writable(self.app / "data")

    def test_a_writable_directory_cannot_hold_registry_trust_files(self):
        manifest = '[run]\nentry = "main.wy"\n[registry]\nprotocol = "tuf"\nurl = "https://packages.example.test"\n'
        for field, label in (("root", "the TUF root"), ("trust", "the TUF release trust file"),
                             ("ca-file", "the registry CA file")):
            with self.subTest(field=field):
                self.manifest.write_text(manifest + f'{field} = "data/anchor"\n')
                with self.assertRaisesRegex(self.cli.ProjectError, f"holds {label}"):
                    self.writable(self.app / "data")
                self.writable(self.app / "data/notes")
        self.manifest.write_text(manifest)
        with patch.dict(os.environ, {"SSL_CERT_FILE": str(self.app / "data/roots.pem")}):
            with self.assertRaisesRegex(self.cli.ProjectError, "holds the toolchain's SSL_CERT_FILE"):
                self.writable(self.app / "data")

        from ssl import DefaultVerifyPaths
        defaults = DefaultVerifyPaths(str(self.app / "data/roots.pem"), None, "", "", "", "")
        with patch.dict(os.environ, {"SSL_CERT_FILE": ""}), patch("ssl.get_default_verify_paths", return_value=defaults):
            with self.assertRaisesRegex(self.cli.ProjectError, "holds the default registry CA file"):
                self.writable(self.app / "data")

    def placed(self, root, writable=True, allowed=None, launching=True, app=None):
        """As `run_project` places a granted directory, on a machine of this test's own."""
        home = self.root / "home"
        app = app or self.app
        env = {name: str(path) for name, path in self.places.items()} | {"HOME": str(home)}
        with patch.dict(os.environ, env), patch.object(tempfile, "tempdir", str(home / "tmp")):
            self.cli.check_directory(str(root))
            protected, stores = self.cli.protected_paths(app / "witchy.toml", app / "main.wy", app / "main.grants.toml", self.tools)
            said, examined = self.cli.check_root_place(str(root), writable, app / "witchy.toml", protected, stores, allowed or {}, launching)
            # The directory examined is named to the engine by what it is, not where.
            status = os.stat(root)
            self.assertEqual(examined, (status.st_dev, status.st_ino))
            return said

    def test_a_directory_is_granted_inside_the_project_or_by_the_operators_word(self):
        home = self.root / "home"
        for made in ("tmp", ".ssh", ".config/git", "Library/Python", "work/registry", "work/other/data", "outside/data"):
            (home / made).mkdir(parents=True)
        for made in (".git/hooks", ".cache", "data/.hidden-parent", "store"):
            (self.app / made).mkdir(parents=True)
        (home / "work/other/witchy.toml").write_text('[run]\nentry = "main.wy"\n')
        self.manifest.write_text('[run]\nentry = "main.wy"\n[registry]\npath = "../home/work/registry"\n')
        (self.app / "data/.hidden-parent").rmdir()
        # Inside the project, a directory of data alone: granted, to read or to write, with nothing to add.
        self.assertIsNone(self.placed(self.app / "data"))
        self.assertIsNone(self.placed(self.app / "data/notes", writable=False))
        # Every root an independent review found accepted (2026-11-22), and their kin: refused,
        # for writing and for reading alike, by where it is and not by a list of what it is.
        outside = "A grant document grants a directory strictly inside its project"
        for root, message in ((self.app / ".git", "the component .git begins with `.`"), (self.app / ".git/hooks", "the component .git begins with `.`"),
                              (self.app / ".cache", "begins with `.`"), (self.app, "it is the project directory itself"),
                              (home / "tmp", "it is or holds the temporary directory"), (home, "it is or holds the home directory"),
                              (self.root, "it is or holds the home directory"), (Path("/"), "contains the descriptor-alias tree /dev"),
                              (home / "work/registry", outside), (home / "work/other", outside), (home / "work", outside),
                              (home / ".ssh", "begins with `.`"), (home / ".config", "begins with `.`"), (home / "Library", outside),
                              (Path(os.path.realpath(os.sys.prefix)), outside), (home / "outside/data", outside)):
            for writable in (True, False):
                with self.subTest(root=root, writable=writable), self.assertRaisesRegex(self.cli.ProjectError, message):
                    self.placed(root, writable=writable)
        # Outside the project only by the operator's word at this launch, for exactly this root
        # and this right; a review or a build says so and grants nothing.
        data = home / "outside/data"
        self.assertIn("OUTSIDE the project, named by the operator at this launch (--allow-write-root)", self.placed(data, allowed={"write": [str(data)]}))
        self.assertIn("OUTSIDE the project, named by the operator at this launch (--allow-read-root)", self.placed(data, writable=False, allowed={"read": [str(data)]}))
        self.assertIn(f"a run is refused unless its command line names it, `grimoire run --allow-write-root {data}`", self.placed(data, launching=False))
        for allowed in ({"read": [str(data)]}, {"write": [str(home / "outside")]}, {"write": [str(data) + "/"]}):
            with self.subTest(allowed=allowed), self.assertRaisesRegex(self.cli.ProjectError, outside):
                self.placed(data, allowed=allowed)
        # The operator's word does not lift any other check.
        for root, message in ((home / "work/other", "work/other/witchy.toml: a program is never granted write access to a manifest"),
                              (home / "work/registry", "it holds the project's registry"), (home / ".ssh", "begins with `.`"),
                              (home, "it is or holds the home directory"), (home / "tmp", "it is or holds the temporary directory"),
                              (self.root / "tools", "it holds the compiler")):
            with self.subTest(named=root), self.assertRaisesRegex(self.cli.ProjectError, message):
                self.placed(root, allowed={"write": [str(root)]})

    def test_a_granted_directory_holds_data_alone(self):
        data = self.app / "data"
        def refused(message, writable=True):
            with self.assertRaisesRegex(self.cli.ProjectError, message):
                self.placed(data, writable=writable)
        for name, message in (("notes/deep/.git", "it holds version-control state"), (".env", "holds no entry whose name begins with `.`"),
                              ("notes/witchy.toml", "never granted write access to a manifest"), ("notes/x.policy", "never granted write access"),
                              ("notes/later.wy", "never granted write access"), ("notes/main.grants.toml", "never granted write access"),
                              # Without case, and no name a file system may fold onto one of those (APFS: KELVIN SIGN, LONG S).
                              ("notes/Main.WY", "never granted write access"), ("WITCHY.Toml", "never granted write access"), ("notes/x.Policy", "never granted write access"),
                              ("notes/witchy.loc\u212a", "a name with a character outside ASCII"), ("main.grant\u017f.toml", "a name with a character outside ASCII"),
                              ("notes/caf\u00e9.txt", "a name with a character outside ASCII")):
            with self.subTest(name=name):
                path = data / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.mkdir() if name.endswith(".git") else path.write_text("")
                refused(message)
                # Reading: only version-control state is refused (its configuration holds credentials).
                if name.endswith(".git"):
                    refused(message, writable=False)
                else:
                    self.assertIsNone(self.placed(data, writable=False))
                path.rmdir() if name.endswith(".git") else path.unlink()
        # The rule the engine applies to every name a program gives, stated once.
        for name, writable_too in (("a", True), ("wal.log", True), ("A-b_c.9", True), ("n" * 255, True), ("Main.WY", False), ("witchy.lock", False)):
            self.assertTrue(self.cli.data_name(name, False), name)
            self.assertEqual(self.cli.data_name(name, True), writable_too, name)
        for name in ("", ".", "..", ".env", "a b", "a/b", "caf\u00e9", "witchy.loc\u212a", "a\n", "n" * 256):
            self.assertFalse(self.cli.data_name(name, False), name)
        # The one hidden entry: the engine's staging directory, with what a killed run left in
        # it (the engine clears that before `main`). Anything else there, or anything else
        # under that name, is refused; so is an older engine's temporary beside the data.
        staging = data / ".witchy-staging"
        staging.mkdir(mode=0o700)
        (staging / "60333-81").write_text("torn")
        self.assertIsNone(self.placed(data))
        self.assertEqual((staging / "60333-81").read_text(), "torn")
        # What it holds is the engine's to judge, under its lock (it refuses what it did not
        # make): a scan beside a working engine saw its files come and go and refused a healthy
        # directory. Here only the directory itself is checked: this user's, mode 0700 exactly.
        (staging / "notes.txt").write_text("")
        self.assertIsNone(self.placed(data))
        (staging / "notes.txt").unlink()
        for mode in (0o777, 0o755, 0o750, 0o701):
            with self.subTest(mode=oct(mode)):
                staging.chmod(mode)
                refused("is not this user's alone .owner, and no group or other permission.")
                refused("Do not remove it while a run is using this directory")
        staging.chmod(0o700)
        self.assertIsNone(self.placed(data))
        # An entry gone between the listing and the look (removed, or moved to where this pass
        # has already been) means the pass did not see the tree: the whole scan starts again,
        # and a tree that never settles is refused. Nothing is skipped as clean.
        import unittest.mock
        real, passes = os.scandir, []
        def changing(times):
            def scandir(path):
                entries = list(real(path))
                if os.fspath(path) == str(data):
                    passes.append(path)
                    if len(passes) <= times:
                        (data / "going.txt").unlink(missing_ok=True)   # listed, then gone
                    else:
                        (data / "going.txt").write_text("")
                return _Listing(entries)
            return scandir
        (data / "going.txt").write_text("")
        # The pause between passes is recorded, not waited for.
        slept = []
        self.enterContext(unittest.mock.patch.object(self.cli.time, "sleep", slept.append))
        with unittest.mock.patch.object(self.cli.os, "scandir", changing(1)):
            self.assertIsNone(self.placed(data))
        self.assertEqual(len(passes), 2, "one pass saw an entry vanish; the next saw a quiet tree")
        self.assertEqual([round(pause, 3) for pause in slept], [0.01], "a short pause before the pass again")
        passes.clear()
        def always(path):
            entries = list(real(path))
            if os.fspath(path) == str(data):
                passes.append(path)
                (data / "going.txt").unlink(missing_ok=True)
            return _Listing(entries)
        def restore_then(path):
            (data / "going.txt").write_text("")
            return always(path)
        with unittest.mock.patch.object(self.cli.os, "scandir", restore_then):
            refused("it kept changing while it was checked .5 passes.: another program is writing to it")
        self.assertEqual(len(passes), self.cli.SCAN_TRIES)
        self.assertEqual([round(pause, 3) for pause in slept[1:]], [0.01, 0.02, 0.03, 0.04, 0.05], "a longer pause after each unsettled pass")
        with unittest.mock.patch.object(self.cli.os, "scandir", restore_then), self.assertRaises(self.cli.ProjectError) as changing_tree:
            self.placed(data)
        self.assertNotIn("remove", str(changing_tree.exception))
        # A directory moved mid-scan is not taken as read: a second pass finds what it holds.
        (data / "going.txt").unlink(missing_ok=True)
        (data / "moving").mkdir()
        (data / "moving/later.wy").write_text("")
        moved = []
        def moving(path):
            entries = list(real(path))
            if os.fspath(path) == str(data) and not moved:
                moved.append(1)
                (data / "moving").rename(data / "aaa-moved")
            return _Listing(entries)
        with unittest.mock.patch.object(self.cli.os, "scandir", moving):
            refused("a program is never granted write access to")
        (data / "aaa-moved/later.wy").unlink()
        (data / "aaa-moved").rmdir()
        # A quiet tree beside a busy staging directory: accepted, its contents never listed.
        listed = []
        def busy(path):
            listed.append(os.fspath(path))
            for stale in list(real(staging)):
                os.unlink(stale.path)
            (staging / f"1-{len(listed)}").write_text("")
            return real(path)
        with unittest.mock.patch.object(self.cli.os, "scandir", busy):
            self.assertIsNone(self.placed(data))
        self.assertNotIn(str(staging), listed)
        for stale in staging.iterdir():
            stale.unlink()
        (staging / "60333-81").write_text("torn")
        # A trailing dot is no part of a name to some file systems: not a data name, and
        # `main.wy.` is still a source.
        for name in ("a.", "a.wy.", "witchy.lock.."):
            self.assertFalse(self.cli.data_name(name, False), name)
        self.assertTrue(self.cli.protected_name("Main.WY.") and self.cli.protected_name("witchy.lock.."))
        (data / "later.wy.").write_text("")
        refused("a program is never granted write access to")
        (data / "later.wy.").unlink()
        (staging / "60333-81").unlink()
        staging.rmdir()
        for make in (lambda: staging.symlink_to(data / "notes"), lambda: staging.write_text("")):
            make()
            refused("is not a directory on the granted directory's device")
            staging.unlink()
        (data / ".wal.log.witchy-60333-81").write_text("torn")
        refused("an unfinished write of an engine older than 2026-11-22")
        (data / ".wal.log.witchy-60333-81").unlink()
        # A protected file under another name, a hard link, is found by what it is.
        (self.app / "main.policy").write_text("main.wy: *\n")
        os.link(self.app / "main.policy", data / "notes/innocent.txt")
        refused("notes/innocent.txt is a protected file under another name")
        (data / "notes/innocent.txt").unlink()
        # A link is never followed to look, and ordinary data is no obstacle.
        (data / "notes/away").symlink_to(self.app)
        (data / "notes/a.note").write_text("kept")
        self.assertIsNone(self.placed(data))
        with patch.object(self.cli, "ROOT_ENTRIES", 2):
            refused("more than 2 entries")
        # A project kept in the home directory, or the temporary one, is granted no directory.
        home = self.root / "home"
        for app, label in ((home, "the home directory"), (home / "tmp", "the temporary directory")):
            (app / "data").mkdir(parents=True)
            (app / "witchy.toml").write_text('[run]\nentry = "main.wy"\n')
            with self.subTest(project=label), self.assertRaisesRegex(self.cli.ProjectError, f"the project directory .* is {label}"):
                self.placed(app / "data", app=app)

    def test_each_limit_is_a_key_with_a_default_and_a_line(self):
        cli = self.cli
        self.assertEqual(cli.read_limits(self.manifest),
                         ["--deadline-ms=1000", "--fuel=1000000", "--net-bytes=1048576", "--fs-bytes=1048576",
                          "--fs-entries=256", "--output-bytes=65536", "--input-bytes=65536", "--memory-pages=4", "--module-bytes=65536", "--connection-deadline-ms=10000"])
        self.manifest.write_text('[run]\nentry = "main.wy"\ndeadline-ms = "none"\nfuel = 2000000\nfuel-per-connection = 500000\n'
                                 "net-bytes = 1099511627776\nfile-bytes = 0\nfile-entries = 12\noutput-bytes = 4096\nmemory-pages = 8\nmodule-bytes = 131072\n")
        limits = cli.read_limits(self.manifest)
        self.assertEqual(limits, ["--deadline-ms=none", "--fuel=2000000", "--net-bytes=1099511627776", "--fs-bytes=0",
                                  "--fs-entries=12", "--output-bytes=4096", "--input-bytes=65536", "--memory-pages=8", "--module-bytes=131072", "--fuel-per-accept=500000", "--connection-deadline-ms=10000"])
        run = {"net-bytes": 1, "file-bytes": 1, "file-entries": 1, "output-bytes": 1, "memory-pages": 8, "module-bytes": 1, "fuel-per-connection": 1}
        flags = ["--output", "--listen=127.0.0.1:8080", "--dir=/srv/data", "--dir-rights=read,write", "--no-links", "--stream-output", "--plain-output"]
        kept = cli.applicable_limits(limits, flags, run)
        self.assertEqual(kept, [limit for limit in limits if not limit.startswith(("--input-bytes", "--connection-deadline-ms"))])  # no input, no Socket: not passed
        self.assertEqual(cli.describe_grants(flags + kept), [
            "grant: output: the program writes its standard output",
            "grant: listener: accepts connections on 127.0.0.1:8080 (loopback only), and dials nothing",
            "grant: directory: /srv/data, read and write (no symbolic link followed, no other device entered)",
            "limit: no deadline: runs until it ends, is stopped, or its launcher ends",
            "limit: stops after 2000000 instructions",
            "limit: network: 1099511627776 bytes sent and received in the whole run",
            "limit: files: 0 bytes read and written in the whole run",
            "limit: files: 12 files and directories made in the whole run (each one made or moved also costs 256 of the file bytes)",
            "limit: output: 4096 bytes written in the whole run",
            "limit: memory: 8 pages of 64 KiB, the program and its heap together",
            "limit: module: 131072 bytes of code",
            "limit: stops after 500000 instructions between accepted connections (the count starts again at each, and after each whole wait for one)"])
        # Each limit the engine is given is named to it as the operator set it: a `[run]` key.
        named = cli.limit_names(flags + kept)
        self.assertEqual(named, ["--limit-name=--fuel=[run] fuel", "--limit-name=--fuel-per-accept=[run] fuel-per-connection", "--limit-name=--deadline-ms=[run] deadline-ms",
                                 "--limit-name=--net-bytes=[run] net-bytes", "--limit-name=--fs-bytes=[run] file-bytes", "--limit-name=--fs-entries=[run] file-entries",
                                 "--limit-name=--output-bytes=[run] output-bytes"])
        self.assertEqual(cli.limit_names(["--deadline-ms=1000", "--unmetered", "--memory-pages=4"]), ["--limit-name=--deadline-ms=[run] deadline-ms"])
        self.assertEqual(cli.describe_grants(flags + kept + named), cli.describe_grants(flags + kept))
        self.assertIn("grant: directory: /srv/data, write-only (no other device entered)",
                      cli.describe_grants(["--dir=/srv/data", "--dir-rights=write"]))
        # A budget for a grant the run does not have: not passed or shown by default, refused if set.
        defaults = ["--deadline-ms=1000", "--fuel=1000000", "--net-bytes=1048576", "--fs-bytes=1048576", "--fs-entries=256", "--output-bytes=65536",
                    "--memory-pages=4", "--module-bytes=65536"]
        # A directory only read makes no names: the entries limit is for one it may write.
        self.assertEqual(cli.applicable_limits(defaults, ["--dir=/srv/data", "--no-links"], {}), ["--deadline-ms=1000", "--fuel=1000000", "--fs-bytes=1048576", "--memory-pages=4"])
        self.assertEqual(cli.applicable_limits(defaults, [], {}), ["--deadline-ms=1000", "--fuel=1000000", "--memory-pages=4"])
        for key, flags, message in (("net-bytes", ["--output"], r"\[run\]\.net-bytes bounds a \[listen\] grant, and this run has none"),
                                    ("fuel-per-connection", [], r"\[run\]\.fuel-per-connection bounds a \[listen\] grant"),
                                    ("file-bytes", ["--listen=127.0.0.1:1"], r"\[run\]\.file-bytes bounds a \[dirs\] or \[files\] grant"),
                                    ("file-entries", ["--dir=/srv/data"], r"\[run\]\.file-entries bounds a \[dirs\] grant with the right to write"),
                                    ("output-bytes", [], r"\[run\]\.output-bytes bounds the output grant")):
            with self.subTest(key=key), self.assertRaisesRegex(cli.ProjectError, message):
                cli.applicable_limits(limits, flags, {key: 1})

    def test_a_program_for_each_connection_has_its_limits_a_connection(self):
        """`main(connection: Socket)`: the same listener grant, said as what it is, and the
        instruction count and the deadline are each connection's."""
        cli = self.cli
        self.manifest.write_text('[run]\nentry = "main.wy"\ndeadline-ms = "none"\nfuel = 2000000\nfuel-per-connection = 500000\nconnection-deadline-ms = 250\n')
        limits = cli.read_limits(self.manifest)
        run = {"fuel": 2000000, "fuel-per-connection": 500000, "connection-deadline-ms": 250}
        flags = ["--listen=127.0.0.1:8080", "--instance-per-connection"]
        kept = cli.applicable_limits(limits, flags, run)
        self.assertEqual(kept, ["--deadline-ms=none", "--fuel=500000", "--net-bytes=1048576", "--memory-pages=4", "--connection-deadline-ms=250"])
        self.assertEqual(cli.describe_grants(flags + kept), [
            "grant: connections: accepts connections on 127.0.0.1:8080 (loopback only) and runs the program once for each, in a new instance that keeps nothing from the last; dials nothing",
            "limit: no deadline: runs until it ends, is stopped, or its launcher ends",
            "limit: each connection: stops after 500000 instructions (that connection is closed; the server goes on)",
            "limit: network: 1048576 bytes sent and received in the whole run",
            "limit: memory: 4 pages of 64 KiB, the program and its heap together",
            "limit: each connection: stops after 250 ms (that connection is closed; the server goes on)"])
        self.assertEqual(cli.limit_names(flags + kept, run), ["--limit-name=--fuel=[run] fuel-per-connection", "--limit-name=--deadline-ms=[run] deadline-ms",
                                                              "--limit-name=--net-bytes=[run] net-bytes", "--limit-name=--connection-deadline-ms=[run] connection-deadline-ms"])
        # Without the key each instance has `fuel`, and the default deadline is shown.
        self.manifest.write_text('[run]\nentry = "main.wy"\n')
        kept = cli.applicable_limits(cli.read_limits(self.manifest), flags, {})
        self.assertEqual(kept, ["--deadline-ms=1000", "--fuel=1000000", "--net-bytes=1048576", "--memory-pages=4", "--connection-deadline-ms=10000"])
        self.assertEqual(cli.limit_names(flags + kept, {})[0], "--limit-name=--fuel=[run] fuel")
        # Every instance is counted; and the deadline is no key of a program that accepts for itself.
        self.manifest.write_text('[run]\nentry = "main.wy"\nfuel = "unmetered"\n')
        with self.assertRaisesRegex(cli.ProjectError, r'a program run once for each connection has a count for each'):
            cli.applicable_limits(cli.read_limits(self.manifest), flags, {"fuel": "unmetered"})
        with self.assertRaisesRegex(cli.ProjectError, r"\[run\]\.connection-deadline-ms bounds a \[listen\] grant taken as a Socket"):
            cli.applicable_limits(limits, ["--listen=127.0.0.1:8080"], {"connection-deadline-ms": 250})

    def test_the_words_of_a_launch_are_checked_and_shown_escaped(self):
        """`launch_words`: one `--arg=` each, only for a program granted them, within the grant;
        and a word cannot end its `grant:` line or pass for another flag."""
        cli = self.cli
        self.assertEqual(cli.launch_words(None, None), [])
        self.assertEqual(cli.launch_words([], "2:8"), [])
        self.assertEqual(cli.launch_words(["a b", "--listen=0.0.0.0:1"], "2:64"), ["--arg=a b", "--arg=--listen=0.0.0.0:1"])
        for words, grant, message in ((["x"], None, r"gives 1 argument after `--`, and the program takes none"),
                                      (["1", "2", "3"], "2:64", r"gives 3 arguments of 3 bytes after `--`, and the document's \[args\] grants at most 2 words and 64 bytes"),
                                      (["\u00e9\u00e9\u00e9"], "2:5", r"of 6 bytes"),
                                      (["a\0b"], "2:64", r"holds a NUL byte"),
                                      (["\udcff"], "2:64", r"is not UTF-8 text"),
                                      (["x"], "2", r"cannot read"), (["x"], "2:-1", r"cannot read"), (["x"], "\u0662:9", r"cannot read")):
            with self.subTest(words=words, grant=grant), self.assertRaisesRegex(cli.ProjectError, message):
                cli.launch_words(words, grant)
        hostile = ["ok", "x\ngrant: directory: /, read and write", "\x1b[2K\u202egnp", 'say "hi"\\', "--listen=0.0.0.0:80", "--deadline-ms=none"]
        flags = ["--output", "--args=8:256"] + cli.launch_words(hostile, "8:256") + ["--deadline-ms=1000"]
        lines = cli.describe_grants(flags)
        self.assertEqual(lines, [
            "grant: output: the program writes its standard output",
            'grant: arguments: 6 given after `--`: "ok" "x\\ngrant: directory: /, read and write" "\\u001b[2K\\u202egnp" "say \\"hi\\"\\\\" "--listen=0.0.0.0:80" "--deadline-ms=none"',
            "limit: arguments: at most 8 words, 256 bytes in all; a launch with more is refused",
            "limit: stops after 1000 ms"])
        self.assertTrue(all(line.isascii() and line.isprintable() for line in lines))
        # A word is no limit and no grant of the launcher's own bookkeeping either.
        self.assertEqual(cli.limit_names(flags), ["--limit-name=--deadline-ms=[run] deadline-ms"])
        self.assertEqual(cli.applicable_limits(["--net-bytes=5"], flags, {}), [])
        self.assertEqual(cli.describe_grants(["--args=8:256"])[0],
                         "grant: arguments: 0 given after `--` (the words an operator writes after `--` at a launch are passed to main's Args, and to nothing else)")

    def test_every_line_of_the_launchers_own_goes_through_say(self):
        """One function writes the launcher's words to standard error, each line with the one
        prefix a program's output is marked for; nothing else in the launcher or its
        companions writes there, but the relay of another tool's words (before a run)."""
        cli = self.cli
        said = io.StringIO()
        with contextlib.redirect_stderr(said):
            cli.say("grant: output: x")
            cli.say("two\nlines\n")
            cli.say("witchy: not the engine's\n\ngrant: all")
        self.assertEqual(said.getvalue(), "grimoire: grant: output: x\ngrimoire: two\ngrimoire: lines\n"
                                          "grimoire: witchy: not the engine's\ngrimoire: \ngrimoire: grant: all\n")
        root = Path(__file__).resolve().parents[1]
        source = (root / "grimoire").read_text()
        writes = [line.strip() for line in source.splitlines() if "sys.stderr.write" in line or "file=sys.stderr" in line or "print_exception" in line]
        self.assertEqual(writes, ['sys.stderr.write("".join(f"{SAID}{line}\\n" for line in lines))', 'sys.stderr.write(text if text.endswith("\\n") else text + "\\n")'])
        for companion in ("package_tests.py", "witchy_compiler.py", "property_trials.py", "test_sources.py", "witchy_release.py"):
            text = (root / companion).read_text()
            self.assertNotIn("file=sys.stderr", text, companion)
            # The compiler client hands on the compiler's own words; nothing else writes.
            self.assertEqual(text.count("sys.stderr.write"), 1 if companion == "witchy_compiler.py" else 0, companion)

    def test_a_shown_word_is_cut_and_its_padding_written_out(self):
        """A long or padded word cannot wrap text of its own choosing onto a row of its own
        (review 2026-11-22), and a record keeps no word of the launch."""
        cli = self.cli
        self.assertEqual(cli.shown_word("a b"), '"a b"')
        padded = cli.shown_word("x" + " " * 40 + "grant: directory: /")
        self.assertNotIn("  ", padded)
        self.assertEqual(padded, '"x' + "\\u0020" * 40 + 'grant: directory: /"')
        long = cli.shown_word("y" * 5000)
        self.assertEqual(long, '"' + "y" * 96 + '"... (5000 characters)')
        (self.app / "main.wy").write_text("fn main() {}\n")
        self.manifest.write_text('[run]\nentry = "main.wy"\n')
        module = self.app / "main.wasm"
        module.write_bytes(b"\0asm")
        record = cli.capture_build_record(self.manifest, self.app / "main.wy", None,
                                          ["--args=2:64", "--arg=secret word", "--output"], cli.file_digest(module), module, module)
        self.assertEqual(record["host_flags"], ["--args=2:64", "--output"])
        self.assertNotIn("secret", json.dumps(record))

    def test_an_input_is_a_grant_line_and_a_limit(self):
        """`main(input: Console<Read>)` takes the file the grant document names or the launch's
        standard input: each a `grant:` line, bounded by `[run] input-bytes`, which bounds
        nothing (and is refused if set) in a run with no input."""
        cli = self.cli
        self.assertEqual(cli.RUN_LIMITS["input-bytes"], ("--input-bytes", 0, cli.BYTE_BUDGET_MOST, 65_536))
        limits = ["--deadline-ms=1000", "--fuel=1000000", "--input-bytes=65536", "--memory-pages=4"]
        for flags, line, limit in ((["--input-stdin"], "grant: input: this launch's standard input, read as the program asks for it", "limit: input: 65536 bytes in all; the read past them ends the run"),
                                   (["--input=/srv/app/requests.txt"], "grant: input: /srv/app/requests.txt, read whole before the program starts", "limit: input: 65536 bytes; a longer input is refused whole")):
            with self.subTest(flags=flags):
                kept = cli.applicable_limits(limits, flags, {})
                self.assertEqual(kept, limits)
                self.assertEqual(cli.describe_grants(flags + kept), [line, "limit: stops after 1000 ms", "limit: stops after 1000000 instructions",
                                 limit, "limit: memory: 4 pages of 64 KiB, the program and its heap together"])
        self.assertNotIn("--input-bytes=65536", cli.applicable_limits(limits, ["--output"], {}))
        with self.assertRaisesRegex(cli.ProjectError, r"\[run\]\.input-bytes bounds an \[input\] grant, and this run has none"):
            cli.applicable_limits(limits, ["--output"], {"input-bytes": 10})

    def test_standard_input_is_a_stream_with_a_running_budget(self):
        """Standard input is read as the program asks: `input-bytes` past 16 MiB, or
        "unlimited", is for it alone (a file is read whole first), and is shown loudly."""
        cli = self.cli
        self.manifest.write_text('[run]\nentry = "main.wy"\ninput-bytes = "unlimited"\n')
        limits = cli.read_limits(self.manifest)
        self.assertIn("--input-bytes=unlimited", limits)
        kept = cli.applicable_limits(limits, ["--input-stdin"], {"input-bytes": "unlimited"})
        self.assertIn("--input-bytes=unlimited", kept)
        self.assertIn('limit: input: UNLIMITED: no bound on the bytes read from standard input ([run] input-bytes = "unlimited"); the deadline and instructions still hold',
                      cli.describe_grants(["--input-stdin"] + kept))
        self.assertIn("--limit-name=--input-bytes=[run] input-bytes", cli.limit_names(["--input-stdin"] + kept))
        for value in ('"unlimited"', "16777217"):
            self.manifest.write_text(f'[run]\nentry = "main.wy"\ninput-bytes = {value}\n')
            limits = cli.read_limits(self.manifest)
            self.assertIn(f"--input-bytes={value.strip('"')}", cli.applicable_limits(limits, ["--input-stdin"], {"input-bytes": 1}))
            with self.subTest(value=value), self.assertRaisesRegex(cli.ProjectError, r"is for standard input .* an input file is read whole before the program starts, at most 16777216 bytes"):
                cli.applicable_limits(limits, ["--input=/srv/app/requests.txt"], {"input-bytes": 1})
        # Not a word of anything else.
        self.manifest.write_text('[run]\nentry = "main.wy"\ninput-bytes = "lots"\n')
        with self.assertRaisesRegex(cli.ProjectError, r'\[run\]\.input-bytes is a whole number from 0 to 1099511627776 or "unlimited" \(standard input only\)'):
            cli.read_limits(self.manifest)

    def test_each_connection_may_have_its_own_budgets(self):
        """`[run] *-per-connection`: what one connection's instance may use, beside the run's,
        each a `limit: each connection:` line and named to the engine as its key."""
        cli = self.cli
        self.manifest.write_text('[run]\nentry = "main.wy"\ndeadline-ms = "none"\nnet-bytes-per-connection = 65536\nfile-bytes-per-connection = 1048576\n'
                                 'file-entries-per-connection = 4\noutput-bytes-per-connection = 512\n')
        limits = cli.read_limits(self.manifest)
        run = {"net-bytes-per-connection": 1, "file-bytes-per-connection": 1, "file-entries-per-connection": 1, "output-bytes-per-connection": 1}
        flags = ["--output", "--listen=127.0.0.1:8080", "--instance-per-connection", "--dir=/srv/data", "--dir-rights=read,write", "--no-links"]
        kept = cli.applicable_limits(limits, flags, run)
        self.assertEqual(kept[-4:], ["--net-bytes-per-connection=65536", "--fs-bytes-per-connection=1048576", "--fs-entries-per-connection=4", "--output-bytes-per-connection=512"])
        lines = cli.describe_grants(flags + kept)
        for line in ("limit: each connection: 65536 bytes sent and received (that instance sends and receives no more; the server goes on)",
                     "limit: each connection: 1048576 file bytes read and written (that instance reads and writes no more; the server goes on)",
                     "limit: each connection: 4 files and directories made (that instance makes no more; the server goes on)",
                     "limit: each connection: 512 output bytes (that connection is closed; the server goes on)",
                     "limit: network: 1048576 bytes sent and received in the whole run"):
            self.assertIn(line, lines)
        named = cli.limit_names(flags + kept, run)
        for flag, key in (("--net-bytes-per-connection", "net-bytes-per-connection"), ("--fs-bytes-per-connection", "file-bytes-per-connection"),
                          ("--fs-entries-per-connection", "file-entries-per-connection"), ("--output-bytes-per-connection", "output-bytes-per-connection")):
            self.assertIn(f"--limit-name={flag}=[run] {key}", named)
        # A connection's own budget for a program that is run once, or for a grant it lacks: refused.
        for key, flags, message in (("net-bytes-per-connection", ["--listen=127.0.0.1:8080"], r"\[run\]\.net-bytes-per-connection bounds a \[listen\] grant taken as a Socket"),
                                    ("output-bytes-per-connection", ["--listen=127.0.0.1:8080", "--instance-per-connection"], r"bounds the output grant to a program run once for each connection"),
                                    ("file-entries-per-connection", ["--listen=127.0.0.1:8080", "--instance-per-connection", "--dir=/srv/data"], r"with the right to write"),
                                    ("file-bytes-per-connection", ["--output"], r"bounds a \[dirs\] grant to a program run once for each connection")):
            with self.subTest(key=key), self.assertRaisesRegex(cli.ProjectError, message):
                cli.applicable_limits(limits, flags, {key: 1})

    def test_a_run_budget_is_unlimited_only_beside_each_connections_own(self):
        """`"unlimited"` for a whole run's byte budget: only for a program run once for each
        connection, only with that connection's own budget set, and shown as what it is."""
        cli = self.cli
        flags = ["--output", "--listen=127.0.0.1:8080", "--instance-per-connection", "--dir=/srv/data", "--dir-rights=read,write", "--no-links"]
        self.manifest.write_text('[run]\nentry = "main.wy"\nnet-bytes = "unlimited"\nnet-bytes-per-connection = 4096\nfile-bytes = "unlimited"\nfile-bytes-per-connection = 8192\n'
                                 'file-entries = "unlimited"\nfile-entries-per-connection = 2\noutput-bytes = "unlimited"\noutput-bytes-per-connection = 100\n')
        with self.manifest.open("rb") as source:
            run = cli.tomllib.load(source)["run"]
        kept = cli.applicable_limits(cli.read_limits(self.manifest), flags, run)
        for flag in ("--net-bytes=unlimited", "--fs-bytes=unlimited", "--fs-entries=unlimited", "--output-bytes=unlimited"):
            self.assertIn(flag, kept)
        lines = cli.describe_grants(flags + kept)
        for line in ('limit: network: UNLIMITED for the whole run ([run] net-bytes = "unlimited"): only each connection\'s own net-bytes-per-connection bounds it',
                     'limit: files: UNLIMITED for the whole run ([run] file-bytes = "unlimited"): only each connection\'s own file-bytes-per-connection bounds it',
                     'limit: files: UNLIMITED for the whole run ([run] file-entries = "unlimited"): only each connection\'s own file-entries-per-connection bounds it',
                     'limit: output: UNLIMITED for the whole run ([run] output-bytes = "unlimited"): only each connection\'s own output-bytes-per-connection bounds it'):
            self.assertIn(line, lines)
        # Without a connection's own beside it, or for a program run once: refused.
        self.manifest.write_text('[run]\nentry = "main.wy"\noutput-bytes = "unlimited"\n')
        limits = cli.read_limits(self.manifest)
        with self.assertRaisesRegex(cli.ProjectError, r'\[run\]\.output-bytes = "unlimited" needs \[run\]\.output-bytes-per-connection'):
            cli.applicable_limits(limits, flags, {"output-bytes": "unlimited"})
        with self.assertRaisesRegex(cli.ProjectError, r'is for a program run once for each connection'):
            cli.applicable_limits(limits, ["--output"], {"output-bytes": "unlimited"})
        # No other word, and no other key.
        for key, value in (("output-bytes", '"none"'), ("memory-pages", '"unlimited"'), ("deadline-ms", '"unlimited"'), ("file-entries-per-connection", '"unlimited"')):
            self.manifest.write_text(f'[run]\nentry = "main.wy"\n{key} = {value}\n')
            with self.subTest(key=key), self.assertRaises(cli.ProjectError):
                cli.read_limits(self.manifest)

    def test_an_input_is_never_granted_with_secrets_and_a_file_goes_with_its_identity(self):
        cli = self.cli
        document = Path("main.grants.toml")
        requests = self.root / "requests.txt"
        requests.write_text("1 + 2")
        flag = f"--input={requests}"
        status = os.stat(requests)
        # A file goes to the engine with what was examined; the engine opens it without
        # following a link and refuses anything else at the path (its own test:
        # an_input_replaced_since_it_was_examined_is_not_the_grant).
        self.assertEqual(cli.input_grant(flag, document, False, False), [flag, f"--input-id={status.st_dev}:{status.st_ino}"])
        self.assertEqual(cli.input_grant("--input-stdin", document, False, False), ["--input-stdin"])
        self.assertNotIn("input-id", "\n".join(cli.describe_grants([flag, f"--input-id={status.st_dev}:{status.st_ino}"])))
        # With secrets: no input at all, file or standard input.
        for granted in (flag, "--input-stdin"):
            with self.subTest(granted=granted), self.assertRaisesRegex(cli.ProjectError, r"\[input\] grants an input: refused while the program takes secrets"):
                cli.input_grant(granted, document, True, False)
        with self.assertRaisesRegex(cli.ProjectError, "a run is granted one input"):
            cli.input_grant(flag, document, False, True)
        # A link is not followed, here or in the engine; nor is a directory or a missing name an input.
        link = self.root / "link.txt"
        link.symlink_to(requests)
        for other in (link, self.root, self.root / "absent.txt"):
            with self.subTest(other=other), self.assertRaisesRegex(cli.ProjectError, "not a regular file that is there"):
                cli.input_grant(f"--input={other}", document, False, False)

    def test_a_single_granted_file_is_inside_the_project_or_named_by_the_operator(self):
        """A `[files]` or `[input]` file is placed by the rule a granted directory is: on the
        document's word only inside the project, never hidden, never a file a build reads;
        anything else by `--allow-read-file` at the launch."""
        cli = self.cli
        place = lambda path, allowed=(), launching=True: cli.check_file_place(str(path), "file", self.manifest, allowed, launching)
        settings = self.app / "settings.conf"
        settings.write_text("8080\n")
        (self.app / "conf").mkdir()
        (self.app / "conf/a.txt").write_text("")
        self.assertIsNone(place(settings))
        self.assertIsNone(place(self.app / "conf/a.txt"))
        self.assertIsNone(place(self.app / "absent.txt"))
        # Inside, but not data: what a build reads, and anything hidden.
        (self.app / ".env").write_text("")
        (self.app / ".cfg").mkdir()
        (self.app / ".cfg/key").write_text("")
        for name in ("witchy.toml", "witchy.lock", "main.wy", "Main.WY", "main.policy", "main.grants.toml", "main.packages", "lib/other.wy"):
            with self.subTest(name=name), self.assertRaisesRegex(cli.ProjectError, "is not data a program is granted"):
                place(self.app / name)
        for name in (".env", ".cfg/key"):
            with self.subTest(name=name), self.assertRaisesRegex(cli.ProjectError, "begins with `.`: no hidden file is granted"):
                place(self.app / name)
        # Outside: the operator's word at this launch, for exactly that path; a review says so.
        hosts = self.root / "outside.txt"
        hosts.write_text("")
        for outside in (hosts, Path("/etc/hosts"), self.app / "../outside.txt"):
            with self.subTest(outside=outside), self.assertRaisesRegex(cli.ProjectError, "outside the project directory .*`grimoire run --allow-read-file "):
                place(outside)
        self.assertEqual(place(hosts, allowed=[str(hosts)]), "OUTSIDE the project, named by the operator at this launch (--allow-read-file)")
        with self.assertRaisesRegex(cli.ProjectError, "outside the project directory"):
            place(hosts, allowed=[str(self.root / "another.txt")])
        self.assertIn("OUTSIDE the project: a run is refused unless its command line names it", place(hosts, launching=False))
        # A link inside the project to a file outside it is outside; one to the lock is the lock.
        (self.app / "link.txt").symlink_to(hosts)
        with self.assertRaisesRegex(cli.ProjectError, "outside the project directory"):
            place(self.app / "link.txt")
        (self.app / "witchy.lock").write_text("")
        (self.app / "data.txt").symlink_to(self.app / "witchy.lock")
        with self.assertRaisesRegex(cli.ProjectError, "is not data a program is granted"):
            place(self.app / "data.txt")
        # Never the trust store, with the operator's word or without.
        state = self.places["XDG_STATE_HOME"] / "grimoire/trust.json"
        with patch.dict(os.environ, {"XDG_STATE_HOME": str(self.places["XDG_STATE_HOME"])}), self.assertRaisesRegex(cli.ProjectError, "it is in Grimoire's state"):
            place(state, allowed=[str(state)])
        # The line a run shows.
        said = {str(hosts): "OUTSIDE the project, named by the operator at this launch (--allow-read-file)"}
        self.assertEqual(cli.describe_grants([f"--file={hosts}"], None, said)[0], f"grant: file: {hosts}, read-only, {said[str(hosts)]}")
        self.assertEqual(cli.describe_grants([f"--input={hosts}"], None, said)[0], f"grant: input: {hosts}, read whole before the program starts, {said[str(hosts)]}")
        # And an input over its limit is said by the key the operator set.
        self.assertIn("--limit-name=--input-bytes=[run] input-bytes", cli.limit_names(["--input-stdin", "--input-bytes=3"]))

    def test_the_modules_own_memory_is_three_keys_each_a_line(self):
        """`heap-pages`, `slots` and `stack-bytes` are built into the module: read from `[run]`,
        given to the compiler with the review, shown as limits; and the engine's memory limit
        follows from them unless the manifest says more."""
        cli = self.cli
        self.assertEqual(cli.read_sizes({}), (3, 64, 16384))
        self.assertEqual(cli.memory_pages_for((3, 64, 16384)), 4)
        self.assertEqual(cli.describe_sizes((3, 64, 16384), {}, ["--output"]), ["limit: stack: 16384 bytes of call frames (built into the module)"])
        self.manifest.write_text('[run]\nentry = "main.wy"\nheap-pages = 8\nslots = 128\nstack-bytes = 32768\n')
        sizes = cli.read_sizes({"heap-pages": 8, "slots": 128, "stack-bytes": 32768})
        self.assertEqual(sizes, (8, 128, 32768))
        # What the engine checked for these sizes (a module built with them starts at 10 pages, not 9).
        self.assertEqual([cli.memory_pages_for(each) for each in ((8, 128, 16384), (64, 4096, 16384), (5, 1024, 16384))], [10, 78, 7])
        self.assertIn("--memory-pages=10", cli.read_limits(self.manifest))
        self.manifest.write_text('[run]\nentry = "main.wy"\nheap-pages = 8\nslots = 128\nmemory-pages = 12\n')
        self.assertIn("--memory-pages=12", cli.read_limits(self.manifest))
        self.assertEqual(cli.describe_sizes(sizes, {"heap-pages": 8, "slots": 128}, ["--allocator", "--output"]), [
            "limit: heap: 425984 bytes, in 8 pages of 64 KiB of program memory (built into the module)",
            "limit: allocations: 128 alive at once (built into the module)",
            "limit: stack: 32768 bytes of call frames (built into the module)"])
        for key in ("heap-pages", "slots"):
            with self.subTest(key=key), self.assertRaisesRegex(cli.ProjectError, rf"\[run\]\.{key} bounds the allocator grant, and this run has none"):
                cli.describe_sizes(sizes, {key: 8}, ["--output"])

    def test_a_limit_out_of_range_is_refused_not_rounded(self):
        for line, message in (("net-bytes = -1", r"\[run\]\.net-bytes is a whole number from 0 to 1099511627776"),
                              ("net-bytes = 1099511627777", r"\[run\]\.net-bytes is a whole number"),
                              ('file-bytes = "infinite"', r"\[run\]\.file-bytes is a whole number from 0 to 1099511627776 or \"unlimited\""),
                              ("output-bytes = 1.5", r"\[run\]\.output-bytes is a whole number"),
                              ("memory-pages = 3", r"\[run\]\.memory-pages is a whole number from 4 to 32768"),
                              ("memory-pages = true", r"\[run\]\.memory-pages is a whole number"),
                              ("fuel-per-connection = 0", r"\[run\]\.fuel-per-connection is a whole number from 1"),
                              ('fuel = "unmetered"\nfuel-per-connection = 5', r'fuel-per-connection counts instructions, and \[run\]\.fuel is "unmetered"'),
                              ('deadline-ms = "forever"', r'\[run\]\.deadline-ms is a number of milliseconds from 1 to 600000, or "none"'),
                              ("heap = 9", r"invalid \[run\]\.heap\. Expected one of: entry, deadline-ms, fuel, net-bytes"),
                              ("heap-pages = 2", r"\[run\]\.heap-pages is a whole number from 3 to 16384"),
                              ("heap-pages = 16385", r"\[run\]\.heap-pages is a whole number"),
                              ("slots = 100", r"\[run\]\.slots is a power of two from 64 to 1048576"),
                              ("slots = 32", r"\[run\]\.slots is a whole number from 64"),
                              ('slots = "many"', r"\[run\]\.slots is a whole number"),
                              ("stack-bytes = 1028", r"\[run\]\.stack-bytes is a multiple of 8"),
                              ("stack-bytes = 131072", r"\[run\]\.stack-bytes is a whole number from 1024 to 65536"),
                              ("heap-pages = 8\nmemory-pages = 9", r"memory-pages = 9 is less than the 10 pages a module built with this heap-pages and slots has")):
            with self.subTest(line=line):
                self.manifest.write_text(f'[run]\nentry = "main.wy"\n{line}\n')
                with self.assertRaisesRegex(self.cli.ProjectError, message):
                    self.cli.read_limits(self.manifest)

    def test_everything_shown_passes_one_escaper_whoever_wrote_it(self):
        """What a tool Grimoire starts writes to the streams it inherits, and what Grimoire
        prints itself, reach the operator only as plain text: a diagnostic quoting a source
        line cannot recolor the terminal or move the cursor."""
        cli = self.cli
        self.assertEqual(cli.plain_text("ok\tfine\n caf\u00e9"), "ok\tfine\n caf\u00e9")
        self.assertEqual(cli.plain_text("\x1b[31mRED\r\x07\x00\x7f\x9b\u202e\u2066\u200b\u2028\ufeff\udcff"),
                         "\\x1b[31mRED\\x0d\\x07\\x00\\x7f\\x9b\\u{202e}\\u{2066}\\u{200b}\\u{2028}\\u{feff}\\xff")
        out, err = tempfile.TemporaryFile(), tempfile.TemporaryFile()
        kept = os.dup(1), os.dup(2)
        child = "import os; os.write(2, b'main.wy:2:13: W0401: x  // \\x1b[31mRED\\xff\\n'); os.write(1, b'\\x1b[2Jname\\r\\n')"
        try:
            os.dup2(out.fileno(), 1)
            os.dup2(err.fileno(), 2)
            with cli.shown_plain():
                subprocess.run([os.sys.executable, "-c", child], check=True)
                print("grimoire: a package named \x1b]0;title\x07", file=os.sys.stderr)
                # The engine alone is given the real streams.
                self.assertEqual([os.fstat(fd).st_ino for fd in cli.SHOWN], [os.fstat(out.fileno()).st_ino, os.fstat(err.fileno()).st_ino])
            self.assertIsNone(cli.SHOWN)
        finally:
            os.dup2(kept[0], 1)
            os.dup2(kept[1], 2)
            os.close(kept[0])
            os.close(kept[1])
        out.seek(0)
        err.seek(0)
        self.assertEqual(out.read(), b"\\x1b[2Jname\\x0d\n")
        self.assertEqual(err.read(), b"main.wy:2:13: W0401: x  // \\x1b[31mRED\\xff\ngrimoire: a package named \\x1b]0;title\\x07\n")

    def test_a_diagnostic_is_read_in_either_compilers_format(self):
        """`W0401 12..19: message` (the earlier compiler) and `W0401: message`: both are the
        diagnostic at that place, in the expected-refusal check and in the author's paths."""
        native = self.cli.native_tests()
        for line in ("/stage/reject/upper.wy:2:13: W0402 30..41: the bound is not proved", "/stage/reject/upper.wy:2:13: W0402: the bound is not proved"):
            with self.subTest(line=line):
                self.assertEqual(native.primary_diagnostics(["proof failed: lemma `upper`", line, "note: W0402 is about bounds"]), [line])
                self.assertTrue(native.diagnostic_at(line, "/stage/reject/upper.wy", "2", "13", "W0402"))
                for path, row, column, code in (("/stage/reject/upper.wy", "2", "1", "W0402"), ("/stage/reject/upper.wy", "2", "13", "W0401"),
                                                ("/stage/reject/upper.wy", "12", "13", "W0402"), ("reject/upper.wy", "2", "13", "W0402")):
                    self.assertFalse(native.diagnostic_at(line, path, row, column, code))
                rewrite = self.cli.author_names([("/stage/", "tests/")])
                self.assertEqual(rewrite(line), line.replace("/stage/", "tests/"))
        self.assertEqual(native.primary_diagnostics(["W0100 0..0: main.policy: does not fit", "W0100: main.policy: does not fit", "W01000: no", "xW0100: no"]),
                         ["W0100 0..0: main.policy: does not fit", "W0100: main.policy: does not fit"])
        self.assertFalse(native.diagnostic_at("/a.wy:2:13: W0402 trailing words: x", "/a.wy", "2", "13", "W0402"))

    def test_a_case_is_held_to_its_own_time_not_the_hosts_start_up(self):
        """The first case of a run pays the test host's start and the module's compilation in
        its wall time: the deadline is on what the host measured inside the case."""
        native = self.cli.native_tests()

        class Host:
            case_deadline_ms = 100

        rows = [{"id": "0", "case": "first", "status": "pass", "host_ns": 2_000_000, "wall_ns": 180_000_000},
                {"id": "1", "case": "slow", "status": "pass", "host_ns": 100_000_000, "wall_ns": 100_400_000},
                {"id": "2", "case": "stuck-binding", "status": "blocked", "wall_ns": 150_000_000}]
        checked = [(row["id"], {"id": row["case"]}, None) for row in rows]
        with patch.object(native, "dispatch", lambda *_: iter([dict(row) for row in rows])):
            out = list(native.checked_outcomes(checked, None, Host(), {}))
        self.assertEqual([(row["case"], row["status"], row.get("reason")) for row in out],
                         [("first", "pass", None), ("slow", "limit", "100 ms invocation deadline"),
                          ("stuck-binding", "limit", "100 ms invocation deadline")])
        Host.case_deadline_ms = 400
        with patch.object(native, "dispatch", lambda *_: iter([dict(row) for row in rows])):
            self.assertEqual([row["status"] for row in native.checked_outcomes(checked, None, Host(), {})], ["pass", "pass", "blocked"])

    def test_a_review_says_what_changed_since_the_accepted_source(self):
        cli = self.cli
        old = "pub fn limit() -> U64:\n    return 64\n\npub fn spare() -> U64:\n    return 1\n"
        new = "pub fn limit() -> U64:\n    let wide = 128\n    return wide\n\npub fn spare() -> U64:\n    return 1\n\nfn added() -> U64:\n    return 2\n"
        package = self.root / "fetched/source.wy"
        package.parent.mkdir()
        package.write_text(new)
        listing = self.app / "main.packages"
        listing.write_text(f"names 0.1.1: {package} ; requires\nother 1.0.0: {self.app / 'lib/helper.wy'}\n")
        accepted = hashlib.sha384(old.encode()).hexdigest()
        unchanged = hashlib.sha384((self.app / "lib/helper.wy").read_bytes()).hexdigest()
        policy = (f"<standard>/: *\nmain.wy: *\n@names: Allocator ; no unchecked ; digest sha384:{accepted}\n"
                  f"@other/: none ; digest sha384:{unchanged}\n@unpinned: none\n")
        with patch.dict(os.environ, {"XDG_STATE_HOME": str(self.places["XDG_STATE_HOME"])}):
            # No copy of what was accepted: said, with where the new source is to be read whole.
            first = cli.source_changes(policy, listing)
            self.assertEqual(len(first), 1)
            self.assertIn("@names 0.1.1: differs from the accepted source", first[0])
            self.assertIn("this machine kept no copy", first[0])
            kept = cli.keep_source(old.encode())
            self.assertEqual(kept, cli.shown_sources() / f"{accepted}.wy")
            self.assertEqual(kept.parent.stat().st_mode & 0o777, 0o700)
            changes = cli.source_changes(policy, listing)
            candidate = cli.shown_sources() / f"{hashlib.sha384(new.encode()).hexdigest()}.wy"
            self.assertEqual(changes, ["@names 0.1.1: 5 lines added, 1 removed in 2 places since the accepted source; in: limit, added",
                                       f"  compare: diff -u {kept} {candidate}"])
            self.assertEqual(candidate.read_text(), new)
            # A kept copy that is not what its name says is not compared with.
            kept.write_text("pub fn limit() -> U64:\n    return 0\n")
            self.assertIn("kept no copy", cli.source_changes(policy, listing)[0])
            # Accepted and unchanged: nothing to say.
            package.write_text(old)
            self.assertEqual(cli.source_changes(policy, listing), [])

    def test_a_version_not_yet_in_a_snapshot_is_said_to_be_the_operators_to_include(self):
        cli = self.cli
        refusal = "notes-core 0.1.1: refused: notes-core's index, signed by its owner mldsa65:abc, does not list this version\n"
        ran = subprocess.CompletedProcess([], 1, "notes-core 0.1.1: snapshot 2\n", refusal)
        said = io.StringIO()
        with patch.object(cli, "run_coven", lambda command: ran), patch.object(cli, "tool", lambda *_: "coven"), contextlib.redirect_stderr(said):
            fetched = cli.coven_fetch("registry.invalid", "notes-core", "0.1.1", self.root / "into", snapshot=(2, "mldsa65:op", self.root / "kept", "dogfood"))
        self.assertFalse(fetched.ok)
        self.assertIn(refusal, said.getvalue())
        self.assertIn("the operator has not yet snapshotted it (`coven snapshot`), or it was never published", said.getvalue())
        self.assertIn("snapshot 2 of the registry", said.getvalue())

    def test_new_service_and_tool_write_what_the_other_commands_read(self):
        """A service's and a tool's project: what they grant is what main takes (the listener as
        a connection and a writable directory inside the project; arguments and standard input),
        their limits are read, and the shapes cannot be asked for together."""
        cli = self.cli
        with contextlib.chdir(self.root), contextlib.redirect_stdout(io.StringIO()):
            cli.new_project("tide-desk", library=False, shape="service")
            cli.new_project("tide-pipe", library=False, shape="tool")
            self.assertEqual(sorted(p.name for p in Path("tide-desk").iterdir()), ["data", "main.grants.toml", "main.policy", "main.wy", "reply.wy", "test.wy", "witchy.toml"])
            self.assertTrue(Path("tide-desk/data").is_dir())
            entry, document, grants = cli.read_project(Path("tide-desk/witchy.toml").resolve())
            granted = cli.tomllib.loads(document.read_text())
            self.assertEqual(granted["listen"], {"connection": "127.0.0.1:8080"})
            self.assertEqual(cli.read_directory_grants(document, granted), {"data": ("data", "read,write")})
            self.assertIn("fn main(connection: Socket, data: Dir, allocator: Allocator):", entry.read_text())
            limits = cli.read_limits(Path("tide-desk/witchy.toml"))
            self.assertIn("--deadline-ms=none", limits)
            self.assertIn("--connection-deadline-ms=10000", limits)
            entry, document, grants = cli.read_project(Path("tide-pipe/witchy.toml").resolve())
            self.assertEqual(cli.tomllib.loads(document.read_text()), {"args": {"args": {"count": 1, "max-bytes": 256}}, "input": {"input": {"stdin": True}}})
            self.assertEqual((grants.get("output"), grants.get("allocator")), (True, True))
            self.assertIn("fn main(args: Args, input: Console<Read>, console: Console<Write>, allocator: Allocator):", entry.read_text())
            # Each scaffold may be drafted over by `review --write-policy`, as the executable's is.
            for name in ("tide-desk", "tide-pipe"):
                self.assertIn(Path(name, "main.policy").read_text(), cli.SCAFFOLD_POLICIES)
        parser = cli.argument_parser(native=False)
        for flags in (["--service", "--tool"], ["--lib", "--service"], ["--lib", "--tool"], ["--exe", "--tool"]):
            with self.subTest(flags=flags), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                parser.parse_args(["new", "x", *flags])

    def test_a_reviewed_record_is_exactly_its_inputs_and_the_lines_shown(self):
        """What `review --accept` records for `run --quiet`: a change to the manifest, the grant
        document, the lock (or its absence) or any line shown is another record, and a kept
        file is believed only if it holds those very bytes."""
        cli = self.cli
        lines = ["grant: output: the program writes its standard output", "limit: stops after 1000 ms"]
        with patch.dict(os.environ, {"XDG_STATE_HOME": str(self.places["XDG_STATE_HOME"])}):
            record = cli.reviewed_grants(self.manifest, self.document, lines)
            self.assertIsNone(cli.was_reviewed(record))
            digest = cli.record_reviewed(record)
            self.assertEqual(digest, hashlib.sha384(record).hexdigest())
            self.assertEqual(cli.was_reviewed(record), digest)
            self.assertEqual(cli.reviewed_folder().stat().st_mode & 0o777, 0o700)
            others = {"a line more": cli.reviewed_grants(self.manifest, self.document, lines + ["limit: stack: 16384 bytes"]),
                      "the lines reordered": cli.reviewed_grants(self.manifest, self.document, lines[::-1]),
                      "no document": cli.reviewed_grants(self.manifest, None, lines)}
            self.document.write_text(self.document.read_text() + "# edited\n")
            others["the document edited"] = cli.reviewed_grants(self.manifest, self.document, lines)
            (self.app / "witchy.lock").write_text("# a lock\n")
            others["a lock"] = cli.reviewed_grants(self.manifest, self.document, lines)
            self.manifest.write_text(self.manifest.read_text() + "fuel = 2\n")
            others["the manifest edited"] = cli.reviewed_grants(self.manifest, self.document, lines)
            for what, other in others.items():
                with self.subTest(what=what):
                    self.assertNotEqual(other, record)
                    self.assertIsNone(cli.was_reviewed(other))
            # Under its name, other bytes or a link are no record.
            kept = cli.reviewed_folder() / digest
            kept.write_bytes(record + b"more")
            self.assertIsNone(cli.was_reviewed(record))
            elsewhere = self.root / "elsewhere"
            elsewhere.write_bytes(record)
            kept.unlink()
            kept.symlink_to(elsewhere)
            self.assertIsNone(cli.was_reviewed(record))

    def test_new_writes_a_project_and_replaces_nothing(self):
        cli = self.cli
        with contextlib.chdir(self.root), contextlib.redirect_stdout(io.StringIO()):
            cli.new_project("tide-table", library=False)
            cli.new_project("tide-core", library=True)
            self.assertEqual(sorted(p.name for p in Path("tide-table").iterdir()), ["main.grants.toml", "main.policy", "main.wy", "witchy.toml"])
            self.assertEqual(sorted(p.name for p in Path("tide-core").iterdir()), ["test.wy", "tide_core.wy", "witchy.toml"])
            # What it writes is what the other commands read: no unknown key, nothing granted unused.
            entry, document, grants = cli.read_project(Path("tide-table/witchy.toml").resolve())
            self.assertEqual((entry.name, document.name, grants.get("output"), grants.get("allocator")), ("main.wy", "main.grants.toml", True, None))
            self.assertEqual(cli.read_limits(Path("tide-table/witchy.toml"))[:2], ["--deadline-ms=1000", "--fuel=1000000"])
            self.assertEqual(cli.read_directory_grants(document, cli.tomllib.loads(document.read_text())), {})
            for name, message in (("tide-table", "exists"), ("Tide", "a project name is"), ("a/b", "a project name is"), ("..", "a project name is")):
                with self.subTest(name=name), self.assertRaisesRegex(cli.ProjectError, message):
                    cli.new_project(name, library=False)
            Path("dangling").symlink_to("nowhere")
            with self.assertRaisesRegex(cli.ProjectError, "exists"):
                cli.new_project("dangling", library=True)


if __name__ == "__main__":
    unittest.main()
