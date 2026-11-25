"""The canonical release (witchy_release.py), called directly: golden vectors, the one encoding
of a set of files, every path rule, both formats beside readers from before the second one,
packing from a directory and unpacking into one. No compiler or engine runs here. The same file
is in Coven and in Grimoire, with the codec it tests.
Run: python3 test_release_format.py"""
import hashlib
import importlib.util
import itertools
import os
from pathlib import Path
import re
import stat
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
CODEC = next(folder / "witchy_release.py" for folder in (HERE, HERE.parent) if (folder / "witchy_release.py").is_file())


def load_codec():
    spec = importlib.util.spec_from_file_location("witchy_release_under_test", CODEC)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


codec = load_codec()

# Golden vectors. Every digest below was computed with `shasum -a 384`, not with the codec.
ONE = b"pub fn one() -> U64:\n    return 1\n"
V1 = b"witchy-package 1\nname lib\nversion 1.0.0\nrequires base 2.0.0\n\n" + ONE
V1_DIGEST = "sha384:1c9e6062d6f206104e9f6a13373766d1dca344c9629583b2eee9b4569f52e750e4e171b2bb3a76a1787900451f31c295"
LIB = b'import "util/text.wy"\nimport "names.wy"\n\npub fn longest() -> U64:\n    return names.limit() + text.width()\n'
NAMES = b"pub fn limit() -> U64:\n    return 64\n"
TEXT = b'import "../names.wy"\n\npub fn width() -> U64:\n    return names.limit()\n'
FILES = {"lib.wy": LIB, "names.wy": NAMES, "util/text.wy": TEXT}
V2_RECORD = (b"witchy-package 2\nname lib\nversion 1.1.0\nrequires base 2.0.0\nentry lib.wy\n"
             b"file lib.wy 106 sha384:508d76ba985c1eb058b8c27d177cced7dc6c0718dbdabb7eb17d6192656d2810e5b4b61bd6480f2295beb298ad250a20\n"
             b"file names.wy 37 sha384:160725c4359ca38a165bf880f9fe076806f50fe5ba3cd1f0d2a5f079f742f6756dea886770f3de0d03e416bb49fbe558\n"
             b"file util/text.wy 70 sha384:a60b30f055a3d6f8e66976362c2af59d8071bbdb93d65ea92059d1c3469fb84b1b78fb5eeba5af67bce51e2c80e4bd91\n\n")
V2 = V2_RECORD + LIB + NAMES + TEXT
V2_DIGEST = "sha384:ec0cd627c126902d5e931ef49382967867bd690b24c57d870cf5c301dd9146b838f12d303930997da1039093d1fa8578"


def pin(data):
    return "sha384:" + hashlib.sha384(data).hexdigest()


def raw(files, entry="lib.wy", name="lib", version="1.1.0", requires=(), order=None, lines=None):
    """A version 2 payload written without the codec: `files` in the order given (or `order`),
    with true lengths and digests unless `lines` replaces the record's file lines."""
    order = list(files) if order is None else order
    listed = lines if lines is not None else [f"file {path} {len(files[path])} {pin(files[path])}" for path in order]
    head = ["witchy-package 2", f"name {name}", f"version {version}", *[f"requires {r}" for r in requires], f"entry {entry}", *listed]
    return ("\n".join(head) + "\n\n").encode() + b"".join(files[path] for path in order)


def chain(*paths):
    """Files in which the first imports the rest, so that all are reached."""
    files = {paths[0]: "".join(f'import "{path}"\n' for path in paths[1:]).encode() + b"pub fn f() -> U64:\n    return 1\n"}
    files.update({path: b"pub fn g() -> U64:\n    return 2\n" for path in paths[1:]})
    return files


def old_coven_record_check(raw_record, name, version):
    """Coven's `check_package_record` as it was before the second format (351fdd5), verbatim in
    what it decides: the reader a registry of that age applies to an uploaded record."""
    lines = raw_record.decode("ascii").splitlines()
    requires = {}
    for line in lines[3:-1]:
        fields = line.split(" ")
        if len(fields) != 3 or fields[0] != "requires":
            raise ValueError("invalid package requirement")
        if not codec.NAME.fullmatch(fields[1]) or not codec.VERSION.fullmatch(fields[2]) or fields[1] == name or fields[1] in requires:
            raise ValueError("invalid duplicate or self dependency")
        requires[fields[1]] = fields[2]
    canonical = "\n".join(["witchy-package 1", f"name {name}", f"version {version}",
                           *[f"requires {d} {e}" for d, e in sorted(requires.items())]]) + "\n\n"
    if raw_record != canonical.encode():
        raise ValueError("invalid canonical package metadata")
    return requires


def old_grimoire_read_record(text, name, version):
    """Grimoire's `read_record` as it was before the second format (8839792): the reader a
    client of that age applies to a fetched record before anything is compiled."""
    try:
        lines = text.decode("ascii").split("\n")
    except UnicodeDecodeError:
        lines = []
    head, rest = lines[:3], lines[3:]
    requires = {}
    valid = head == ["witchy-package 1", f"name {name}", f"version {version}"] and rest[-2:] == ["", ""]
    for line in rest[:-2] if valid else []:
        words = line.split(" ")
        if (len(words) != 3 or words[0] != "requires" or not codec.NAME.fullmatch(words[1]) or not codec.VERSION.fullmatch(words[2])
                or words[1] == name or (requires and words[1] <= max(requires))):
            valid = False
            break
        requires[words[1]] = words[2]
    if not valid:
        raise ValueError(f"{name} {version}: the registry's package record is not the record of {name} {version}")
    return requires


class Format(unittest.TestCase):
    def refused(self, why, payload, name=None, version=None):
        with self.assertRaisesRegex(codec.ReleaseError, why):
            codec.parse_payload(payload, name, version)

    def test_golden_vectors_and_round_trip(self):
        self.assertEqual((pin(V1), pin(V2)), (V1_DIGEST, V2_DIGEST))
        one = codec.parse_payload(V1, "lib", "1.0.0")
        self.assertEqual((one.format, one.digest, one.requires, one.entry, one.files), (1, V1_DIGEST, {"base": "2.0.0"}, "source.wy", [("source.wy", ONE)]))
        # One file is the first format whatever it is called: its bytes and digest are what they
        # were before there was a second format.
        for entry in ("lib.wy", "Reader.wy", "source.wy"):
            self.assertEqual(codec.pack("lib", "1.0.0", {"base": "2.0.0"}, entry, {entry: ONE}), V1)
        two = codec.parse_payload(V2, "lib", "1.1.0")
        self.assertEqual((two.format, two.digest, two.requires, two.entry, two.files), (2, V2_DIGEST, {"base": "2.0.0"}, "lib.wy", list(FILES.items())))
        self.assertEqual(two.listing(), [(path, len(data), pin(data)) for path, data in FILES.items()])
        self.assertEqual((two.record, two.body, two.payload), (V2_RECORD, LIB + NAMES + TEXT, V2))
        self.assertEqual(codec.split(V2), (V2_RECORD, LIB + NAMES + TEXT))
        # The same files in any order of presentation are the same bytes.
        for order in itertools.permutations(FILES):
            self.assertEqual(codec.pack("lib", "1.1.0", {"base": "2.0.0"}, "lib.wy", {path: FILES[path] for path in order}), V2)
        # What parses packs back to itself.
        for release in (one, two):
            self.assertEqual(codec.pack(release.name, release.version, release.requires, release.entry, dict(release.files)), release.payload)

    def test_a_release_is_the_release_of_its_coordinate(self):
        for name, version in (("other", "1.1.0"), ("lib", "1.1.1")):
            self.refused("not the record of", V2, name, version)
            self.refused("not the record of", V1, name, version)
        with self.assertRaisesRegex(codec.ReleaseError, "invalid package coordinate"):
            codec.pack("Lib", "1.0.0", {}, "lib.wy", {"lib.wy": ONE})
        with self.assertRaisesRegex(codec.ReleaseError, "self dependency"):
            codec.pack("lib", "1.0.0", {"lib": "1.0.0"}, "lib.wy", {"lib.wy": ONE})

    def test_every_path_rule(self):
        """Each path alone, and as a file of an otherwise well-formed release."""
        bad = {
            "": "not empty",
            "/etc/passwd.wy": "relative",
            "a//b.wy": "empty component",
            "a/": "empty component",
            "./a.wy": r"`\.` or `\.\.`",
            "a/./b.wy": r"`\.` or `\.\.`",
            "../a.wy": r"`\.` or `\.\.`",
            "a/../b.wy": r"`\.` or `\.\.`",
            "a/..": r"`\.` or `\.\.`",
            ".hidden.wy": "begins with a dot",
            "a/.git/b.wy": "begins with a dot",
            "A.wy": "lower case",
            "sub/Mod.wy": "lower case",
            "a\\b.wy": "separates components",
            "a\0b.wy": "control",
            "a\nfile b.wy": "control",
            "a\tb.wy": "control",
            "a\x7fb.wy": "control",
            "café.wy": "ASCII",              # NFC
            "café.wy": "ASCII",             # NFD of the same name
            "‮gnp.wy": "ASCII",              # right-to-left override
            "a‏b.wy": "ASCII",               # right-to-left mark
            "a⁦b.wy": "ASCII",               # isolate
            "ａ.wy": "ASCII",                 # fullwidth a
            "a b.wy": "a component is",
            "a..wy": "a component is",
            "a-.wy": "a component is",
            "-a.wy": "a component is",
            "a.wy.": "a component is",
            "a:b.wy": "a component is",
            "a*.wy": "a component is",
            "a.txt": "only `.wy`",
            "license": "only `.wy`",
            "a.wy/b": "only `.wy`",
            ".wy": "begins with a dot",
            "a.WY": "lower case",
            "x" * 61 + ".wy" + "": None,          # 64 bytes: allowed
            "x" * 62 + ".wy": "at most 64 bytes",
            "/".join(["d"] * 8) + "/a.wy": "at most 8 components",
            "/".join(["d" * 60] * 5) + "/a.wy": "at most 255 bytes",
        }
        for path, why in bad.items():
            with self.subTest(path=path):
                if why is None:
                    self.assertEqual(codec.checked_path(path), path)
                    continue
                with self.assertRaisesRegex(codec.ReleaseError, why):
                    codec.checked_path(path)
                if path and path.isascii() and "\n" not in path and " " not in path:
                    files = {"lib.wy": f'import "{path}"\n'.encode(), path: b"pub fn g() -> U64:\n    return 2\n"}
                    with self.assertRaises(codec.ReleaseError):
                        codec.parse_payload(raw(files, order=sorted(files)))
                    with self.assertRaises(codec.ReleaseError):
                        codec.pack("lib", "1.1.0", {}, "lib.wy", files)
        for path in (None, 7, b"a.wy"):
            with self.assertRaises(codec.ReleaseError):
                codec.checked_path(path)
        for path in ("a.wy", "util/text.wy", "a_b/c-d.e.wy", "0.wy", "_x.wy", "/".join(["d"] * 7) + "/a.wy"):
            self.assertEqual(codec.checked_path(path), path)

    def test_the_rules_on_a_set_of_paths(self):
        cases = [
            (["lib.wy", "lib.wy"], "listed once each"),
            (["names.wy", "lib.wy"], "in the order of their bytes"),
            (["a.wy", "a.wy/b.wy"], "both a file and a directory"),
            (["a.wy", "a.wy/sub/b.wy"], "both a file and a directory"),
            (["a.wy", "A.wy"], "lower case"),
            (["lib.wy"], "holds 2 to 64 files"),
            ([], "holds 2 to 64 files"),
            ([f"f{number:03}.wy" for number in range(65)], "holds 2 to 64 files"),
        ]
        for paths, why in cases:
            with self.subTest(paths=paths[:3]), self.assertRaisesRegex(codec.ReleaseError, why):
                codec.checked_file_set(paths)
        # Folding and normalization are checked on their own, whatever the grammar allows.
        grammar, codec.checked_path = codec.checked_path, lambda path: path
        try:
            for pair in (["a.wy", "A.wy"], ["café.wy", "café.wy"], ["straße.wy", "strasse.wy"]):
                with self.subTest(pair=pair), self.assertRaisesRegex(codec.ReleaseError, "same path under case folding or normalization"):
                    codec.checked_file_set(sorted(pair))
        finally:
            codec.checked_path = grammar
        self.assertEqual(len(codec.checked_file_set([f"f{number:03}.wy" for number in range(64)])), 64)
        # In a release: an entry that is not listed, or not at the top of the package.
        files = chain("lib.wy", "sub/a.wy")
        self.refused("is not one of its files", raw(files, entry="other.wy"))
        self.refused("at the top of the package", raw({"sub/a.wy": b'import "../lib.wy"\n', "lib.wy": b""}, entry="sub/a.wy", order=["lib.wy", "sub/a.wy"]))
        self.refused("in the order of their bytes", raw(files, order=["sub/a.wy", "lib.wy"]))
        self.refused("listed once each", raw(files, order=["lib.wy", "sub/a.wy", "sub/a.wy"]))
        self.refused("holds 2 to 64 files", raw({"lib.wy": ONE}))
        many = chain("lib.wy", *[f"f{number:03}.wy" for number in range(64)])
        self.refused("holds 2 to 64 files", raw(many, order=sorted(many)))

    def test_one_set_of_files_has_one_byte_string(self):
        """Every other spelling of the golden release is refused."""
        lines = V2_RECORD.decode().split("\n")
        def spelled(change):
            changed = list(lines)
            change(changed)
            return "\n".join(changed).encode() + LIB + NAMES + TEXT
        def swap(i, j):
            return lambda l: l.__setitem__(slice(None), [*l[:i], l[j], *l[i + 1:j], l[i], *l[j + 1:]])
        attempts = {
            "leading zero": lambda l: l.__setitem__(5, l[5].replace(" 106 ", " 0106 ")),
            "plus sign": lambda l: l.__setitem__(5, l[5].replace(" 106 ", " +106 ")),
            "hex length": lambda l: l.__setitem__(5, l[5].replace(" 106 ", " 0x6a ")),
            "two spaces": lambda l: l.__setitem__(5, l[5].replace("file ", "file  ")),
            "tab": lambda l: l.__setitem__(5, l[5].replace("file ", "file\t")),
            "trailing space": lambda l: l.__setitem__(5, l[5] + " "),
            "carriage return": lambda l: l.__setitem__(5, l[5] + "\r"),
            "carriage return after entry": lambda l: l.__setitem__(4, l[4] + "\r"),
            "upper-case digest": lambda l: l.__setitem__(5, l[5].upper().replace("FILE LIB.WY", "file lib.wy").replace("SHA384", "sha384")),
            "another algorithm": lambda l: l.__setitem__(5, l[5].replace("sha384:", "sha512:")),
            "path with ./": lambda l: l.__setitem__(5, l[5].replace("file lib.wy", "file ./lib.wy")),
            "entry with ./": lambda l: l.__setitem__(4, "entry ./lib.wy"),
            "files out of order": swap(5, 6),
            "entry after files": lambda l: l.__setitem__(slice(4, 8), [*l[5:8], l[4]]),
            "two entries": lambda l: l.insert(5, "entry names.wy"),
            "no entry": lambda l: l.pop(4),
            "requires after entry": swap(3, 4),
            "comment line": lambda l: l.insert(4, "# note"),
            "unknown field": lambda l: l.insert(4, "license MIT"),
            "blank line inside": lambda l: l.insert(4, ""),
            "extra file line": lambda l: l.insert(8, "file zz.wy 0 " + pin(b"")),
            "leading space": lambda l: l.__setitem__(0, " witchy-package 2"),
            "format 2.0": lambda l: l.__setitem__(0, "witchy-package 2.0"),
            "format 02": lambda l: l.__setitem__(0, "witchy-package 02"),
            "format 3": lambda l: l.__setitem__(0, "witchy-package 3"),
            "name twice": lambda l: l.insert(2, "name lib"),
            "version v": lambda l: l.__setitem__(2, "version v1.1.0"),
            "version leading zero": lambda l: l.__setitem__(2, "version 1.01.0"),
        }
        for label, change in attempts.items():
            with self.subTest(label), self.assertRaises(codec.ReleaseError):
                codec.parse_payload(spelled(change))
        self.refused("use a newer tool", spelled(attempts["format 3"]))
        two = [b"requires base 2.0.0\nrequires apple 1.0.0\n", b"requires base 2.0.0\nrequires base 2.0.0\n", b"requires base 2.0.0\nrequires base 2.0.1\n",
               b"requires lib 1.0.0\n", b"requires base 2.0\n", b"requires Base 2.0.0\n", b"requires base  2.0.0\n"]
        for requires in two:
            with self.subTest(requires=requires):
                self.refused("", V2.replace(b"requires base 2.0.0\n", requires))
                self.refused("", V1.replace(b"requires base 2.0.0\n", requires))
        # The body is exactly the listed files: nothing after, nothing missing, each where its
        # line says.
        self.refused("not exactly the files", V2 + b"\n")
        self.refused("not exactly the files", V2[:-1])
        self.refused("not exactly the files", V2_RECORD)
        self.refused("does not have the digest", V2_RECORD.replace(b" 106 ", b" 105 ").replace(b" 37 ", b" 38 ") + LIB + NAMES + TEXT)
        self.refused("does not have the digest", V2_RECORD + NAMES + LIB + TEXT)
        self.refused("incomplete package record", V2_RECORD[:-1])
        self.refused("", V2_RECORD[:-2] + b"\r\n\r\n" + LIB + NAMES + TEXT)
        # The record a slot holds is the record, whole: bytes moved across the split are refused.
        with self.assertRaises(codec.ReleaseError):
            codec.parse(V2_RECORD + LIB[:10], LIB[10:] + NAMES + TEXT)
        with self.assertRaises(codec.ReleaseError):
            codec.parse(V2_RECORD[:-1], b"\n" + LIB + NAMES + TEXT)
        with self.assertRaises(codec.ReleaseError):
            codec.parse(V1[:len(V1) - len(ONE)] + ONE[:5], ONE[5:])
        # Neither format is read as the other.
        self.refused("", V2.replace(b"witchy-package 2", b"witchy-package 1"))
        self.refused("", V1.replace(b"witchy-package 1", b"witchy-package 2"))
        self.refused("", b"witchy-package 1\nname lib\nversion 1.0.0\nentry lib.wy\n\n" + ONE)
        self.refused("", b"witchy-package 1\nname lib\nversion 1.0.0\n\xff\n\n" + ONE)
        self.refused("at most 1048576 bytes", V1 + b" " * (1 << 20))

    def test_each_file_has_the_digest_its_record_lists(self):
        tampered = V2.replace(b"return 64", b"return 65")
        self.assertEqual(len(tampered), len(V2))
        self.refused(r"file 'names.wy' does not have the digest", tampered)
        # With its line rewritten the release parses, and is another release: the digest the
        # owner signed and the lock pins is of the bytes before.
        again = tampered.replace(pin(NAMES).encode(), pin(NAMES.replace(b"return 64", b"return 65")).encode())
        self.assertNotEqual(codec.parse_payload(again).digest, V2_DIGEST)
        self.refused("does not have the digest", V2.replace(pin(TEXT).encode(), pin(b"").encode()))

    def test_a_release_is_exactly_what_its_entry_reaches(self):
        function = b"pub fn g() -> U64:\n    return 2\n"
        cases = {
            "which nothing in it imports": {"lib.wy": b'import "a.wy"\n', "a.wy": function, "extra.wy": function},
            "which is not a file of the release": {"lib.wy": b'import "a.wy"\nimport "missing.wy"\n', "a.wy": function},
            "leaves the package's directory": {"lib.wy": b'import "a.wy"\nimport "../outside.wy"\n', "a.wy": function},
            "leaves the package's directory'": {"lib.wy": b'import "sub/a.wy"\n', "sub/a.wy": b'import "../../outside.wy"\n'},
            "relative to itself": {"lib.wy": b'import "a.wy"\nimport "/etc/passwd"\n', "a.wy": function},
            "relative to itself'": {"lib.wy": b'import "a.wy"\nimport ""\n', "a.wy": function},
            "which the release does not require": {"lib.wy": b'import "a.wy"\nimport "@other"\n', "a.wy": function},
            "a source is UTF-8": {"lib.wy": b'import "a.wy"\n', "a.wy": b"\xff\xfe"},
            # An import after the first declaration is not one, so the file is not reached.
            "which nothing in it imports'": {"lib.wy": b'pub fn f() -> U64:\n    return 1\nimport "a.wy"\n', "a.wy": function},
            "which is not a file of the release'": {"lib.wy": b'import "A.wy"\n', "a.wy": function},
        }
        for why, files in cases.items():
            with self.subTest(why):
                self.refused(why.rstrip("'"), raw(files, order=sorted(files)))
                with self.assertRaises(codec.ReleaseError):
                    codec.pack("lib", "1.1.0", {}, "lib.wy", files)
        required = {"lib.wy": b'import "a.wy"\nimport "@base"\nimport console\n', "a.wy": function}
        self.assertEqual(codec.parse_payload(raw(required, requires=["base 2.0.0"], order=sorted(required))).requires, {"base": "2.0.0"})
        cycle = {"lib.wy": b'import "./a.wy"\n', "a.wy": b'import "lib.wy"\nimport "sub/../a.wy"\n'}
        self.assertEqual(codec.parse_payload(raw(cycle, order=sorted(cycle))).format, 2)

    def test_imports_are_read_as_the_compiler_reads_a_header(self):
        source = ('// a comment\n\nimport console\nimport "a.wy"  \t\nimport "sub/b.wy"\r\nfrom a import T\nimport json_tree\n'
                  'import "@other"\n  import "indented.wy"\nimport "late.wy"\n')
        self.assertEqual(codec.imports(source.encode()), ["a.wy", "sub/b.wy", "@other"])
        self.assertEqual(codec.imports(b'import "a.wy"\nfn main():\n    return\nimport "b.wy"\n'), ["a.wy"])
        self.assertEqual(codec.imports(b'import "\nimport "a.wy"\n'), [])
        self.assertEqual(codec.imports(b'import Upper\nimport "a.wy"\n'), [])
        self.assertEqual(codec.imports(b'\xef\xbb\xbfimport "a.wy"\n'), [])
        self.assertEqual(codec.imports(b'import "a.wy"\x1f\nimport "b.wy"\n'), [])
        self.assertEqual(codec.imports(b'import ""\n'), [""])
        self.assertEqual(codec.imports(b""), [])

    def test_a_reader_from_before_refuses_the_second_format_and_a_new_one_reads_the_first(self):
        record, body = codec.split(V1)
        self.assertEqual(old_coven_record_check(record, "lib", "1.0.0"), {"base": "2.0.0"})
        self.assertEqual(old_grimoire_read_record(record, "lib", "1.0.0"), {"base": "2.0.0"})
        self.assertEqual(codec.parse(record, body, "lib", "1.0.0").requires, {"base": "2.0.0"})
        with self.assertRaisesRegex(ValueError, "invalid package requirement|invalid canonical"):
            old_coven_record_check(V2_RECORD, "lib", "1.1.0")
        with self.assertRaisesRegex(ValueError, "is not the record of lib 1.1.0"):
            old_grimoire_read_record(V2_RECORD, "lib", "1.1.0")
        # The first format's body is its one file, whatever it holds: never read as a listing.
        odd = [b"", b"\n\n", V2, b'import "names.wy"\n', b"\xff", V2_RECORD]
        for body in odd:
            release = codec.parse(record, body)
            self.assertEqual((release.format, release.files, release.digest), (1, [("source.wy", body)], pin(record + body)))
        without = b"witchy-package 1\nname lib\nversion 1.0.0\n\n"
        self.assertEqual(codec.parse(without, ONE).digest, pin(without + ONE))
        self.assertEqual(codec.record("lib", "1.0.0", {}), without)


class Directory(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        self.package = self.root / "package"
        self.package.mkdir()
        for path, data in FILES.items():
            (self.package / path).parent.mkdir(exist_ok=True)
            (self.package / path).write_bytes(data)

    def read(self, entry="lib.wy", excluded=lambda path: False):
        handle = os.open(self.package, os.O_RDONLY | os.O_DIRECTORY)
        try:
            return codec.read_package(handle, entry, excluded)
        finally:
            os.close(handle)

    def refused(self, why, **options):
        with self.assertRaisesRegex(codec.ReleaseError, why):
            self.read(**options)

    def test_a_package_is_read_as_the_files_its_entry_reaches(self):
        (self.package / "unreferenced.wy").write_bytes(b"pub fn spare() -> U64:\n    return 0\n")
        (self.package / "notes.txt").write_text("not a source")
        files = self.read()
        self.assertEqual(files, FILES)
        self.assertEqual(codec.pack("lib", "1.1.0", {"base": "2.0.0"}, "lib.wy", files), V2)
        # One file, named as its author likes, is the first format.
        (self.package / "Solo File.wy").write_bytes(ONE)
        self.assertEqual(self.read("Solo File.wy"), {"Solo File.wy": ONE})
        (self.package / "Upper.wy").write_bytes(b'import "names.wy"\n')
        self.refused("lower case", entry="Upper.wy")

    def test_no_link_or_special_file_is_read(self):
        outside = self.root / "outside.wy"
        outside.write_bytes(b"pub fn secret() -> U64:\n    return 42\n")
        names = self.package / "names.wy"
        names.unlink()
        names.symlink_to(outside)
        self.refused("symbolic link")
        names.unlink()
        names.symlink_to("util/text.wy")  # a link that stays inside is a link all the same
        self.refused("symbolic link")
        names.unlink()
        os.link(outside, names)
        self.refused("more than one name")
        names.unlink()
        os.mkfifo(names)
        self.refused("not a regular file")
        names.unlink()
        names.mkdir()
        self.refused("not a regular file")
        names.rmdir()
        self.refused("is missing")
        names.write_bytes(NAMES)
        (self.root / "elsewhere").mkdir()
        (self.root / "elsewhere/text.wy").write_bytes(TEXT)
        (self.package / "util/text.wy").unlink()
        (self.package / "util").rmdir()
        (self.package / "util").symlink_to(self.root / "elsewhere")
        self.refused("is a link or not a directory")
        # The entry itself.
        (self.package / "entry.wy").symlink_to(outside)
        self.refused("symbolic link", entry="entry.wy")
        for entry in ("../outside.wy", "util/text.wy", "", ".", ".."):
            self.refused("the entry is a file of the package's directory", entry=entry)

    def test_imports_stay_inside_and_test_members_stay_out(self):
        lib = self.package / "lib.wy"
        for written, why in [("../outside.wy", "leaves the package's directory"), ("/etc/hosts", "relative to itself"),
                             ("util/../../outside.wy", "leaves the package's directory"), ("Names.wy", "lower case"),
                             ("names.txt", "only `.wy`"), (".hidden.wy", "begins with a dot"), ("a\\b.wy", "separates components")]:
            lib.write_bytes(f'import "{written}"\n'.encode())
            with self.subTest(written=written):
                self.refused(why)
        lib.write_bytes(b'import "tests/helper.wy"\n')
        (self.package / "tests").mkdir()
        (self.package / "tests/helper.wy").write_bytes(ONE)
        self.refused("not a package test member", excluded=lambda path: path.startswith("tests/"))
        lib.write_bytes(b"".join(f'import "f{number:03}.wy"\n'.encode() for number in range(64)))
        for number in range(64):
            (self.package / f"f{number:03}.wy").write_bytes(b"")
        self.refused("at most 64 files")
        lib.write_bytes(b'import "names.wy"\n')
        (self.package / "names.wy").write_bytes(b" " * (codec.PAYLOAD_BYTES + 1))
        self.refused("larger than a version holds")
        (self.package / "names.wy").write_bytes(b" " * (codec.PAYLOAD_BYTES - 10))
        self.refused("more than a version holds")

    def test_a_release_is_unpacked_into_a_new_directory_file_by_file(self):
        release = codec.parse_payload(V2)
        tree = Path(codec.extract(release, self.root, "tree"))
        self.assertEqual(tree, self.root / "tree")
        found = {path.relative_to(tree).as_posix(): path.read_bytes() for path in tree.rglob("*") if path.is_file()}
        self.assertEqual(found, FILES)
        for path in tree.rglob("*"):
            status = path.lstat()
            self.assertFalse(stat.S_ISLNK(status.st_mode))
            self.assertEqual(stat.S_IMODE(status.st_mode), 0o700 if path.is_dir() else 0o400, path)
            self.assertEqual(status.st_nlink, 1 if path.is_file() else status.st_nlink)
        # Never into a directory that is there, a link to one, or a path of several parts.
        with self.assertRaisesRegex(codec.ReleaseError, "cannot unpack"):
            codec.extract(release, self.root, "tree")
        (self.root / "planted").symlink_to(self.root / "elsewhere")
        with self.assertRaisesRegex(codec.ReleaseError, "cannot unpack"):
            codec.extract(release, self.root, "planted")
        self.assertFalse((self.root / "elsewhere").exists())
        for name in ("a/b", "..", ".", ""):
            with self.assertRaisesRegex(codec.ReleaseError, "new directory of the given one"):
                codec.extract(release, self.root, name)
        one = Path(codec.extract(codec.parse_payload(V1), self.root, "one"))
        self.assertEqual(list(one.iterdir()), [one / "source.wy"])
        self.assertEqual((one / "source.wy").read_bytes(), ONE)
        self.assertEqual(stat.S_IMODE((one / "source.wy").stat().st_mode), 0o400)
        with self.assertRaisesRegex(codec.ReleaseError, "only a parsed release"):
            codec.extract(V2, self.root, "bytes")
        invalid_one = codec.Release(1, "base", "2.0.0", {}, "source.wy", [("other.wy", ONE)], b"", ONE)
        with self.assertRaisesRegex(codec.ReleaseError, "holds exactly its source.wy"):
            codec.extract(invalid_one, self.root, "invalid-one")
        self.assertFalse((self.root / "invalid-one").exists())
        forged = codec.Release(2, "lib", "1.1.0", {}, "lib.wy", [("../escaped.wy", b"x"), ("lib.wy", b"")], b"", b"")
        with self.assertRaises(codec.ReleaseError):
            codec.extract(forged, self.root, "forged")
        self.assertFalse((self.root / "escaped.wy").exists())
        self.assertFalse((self.root / "forged").exists())


if __name__ == "__main__":
    unittest.main()
