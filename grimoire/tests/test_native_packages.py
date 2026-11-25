"""Run the real package entry, compiler and Wasmtime session on small packages."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class NativePackages(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.project = Path(self.temp.name).resolve() / "paired-values"
        shutil.copytree(ROOT / "examples/package-tests", self.project)

    def cli(self, *args, code=0, data=None):
        result = subprocess.run(
            [ROOT / "grimoire", "test", self.project, *args],
            input=data,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, code, result.stderr + result.stdout)
        return result

    def test_move_case_without_moving_its_provider_and_refuse_mismatched_field(self):
        entry = self.project / "test.wy"
        source = entry.read_text()
        member = self.project / "tests/other.wy"
        member.write_text(
            source.replace('"values.wy"', '"../values.wy"')
            .replace('"tests/fixtures.wy"', '"fixtures.wy"')
            .replace("fn adds(", "fn total(")
        )
        entry.write_text('import "tests/other.wy"\n')
        report = json.loads(self.cli("--json").stdout)
        self.assertEqual(
            [(r["case"], r["status"]) for r in report["outcomes"]],
            [("tests/other.wy::total", "pass")],
        )
        self.assertEqual(report["work"]["stores"], 1)
        provider = self.project / "tests/fixtures.wy"
        provider.write_text(
            provider.read_text()
            .replace("expected: U64\n", "expected: Bool\n", 1)
            .replace(", 42)", ", true)")
        )
        refusal = self.cli("--list", code=2).stderr
        self.assertIn("tests/other.wy", refusal)
        self.assertIn("W040", refusal)

    def test_fixture_transfers_two_distinct_owners(self):
        shutil.copyfile(
            ROOT / "tests/fixtures/native-owned.wy", self.project / "test.wy"
        )
        report = json.loads(self.cli("--json").stdout)
        self.assertEqual(
            [(r["case"], r["status"]) for r in report["outcomes"]],
            [("test.wy::number", "pass"), ("test.wy::owned", "pass")],
        )

    def test_required_directory_is_bound_only_under_the_authorized_root(self):
        (self.project / "test.wy").write_text("""import fs
import testing
import test_runner

test fn reads(source: &Dir<Read>) -> Result<(), Failure>:
    let Ok(file): File<Read> = source.open("value") else Err(error):
        return testing.ok(false)
    let value = file.next_byte()
    file.close()
    match value:
        Ok(byte):
            return testing.eq_u64(U8.to_u64(byte), 17)
        Err:
            return testing.ok(false)

fn main(tests: Tests, grants: TestGrants):
    for case in tests.collect():
        if grants.has_root("data"):
            grants.offer_directory(case, "source", "data", "samples")
        match grants.bind(case):
            Bound(invocation):
                invocation.run()
            Blocked(slot):
                continue
""")
        tree = Path(self.temp.name).resolve() / "authorized"
        samples = tree / "samples"
        samples.mkdir(parents=True)
        (samples / "value").write_bytes(bytes([17]))
        for flags in ([], ["--write-root", f"data={tree}"]):
            response = json.loads(self.cli("--json", *flags, code=2).stdout)
            (row,) = response["outcomes"]
            self.assertEqual(
                (row["status"], row["slot"], row["entered"]),
                ("blocked", "source", False),
            )
            self.assertEqual(
                response["work"]["stores"], 1
            )  # Only the scheduler entered.
        (row,) = json.loads(self.cli("--json", "--read-root", f"data={tree}").stdout)[
            "outcomes"
        ]
        self.assertEqual(row["status"], "pass")

    def test_a_case_is_lent_a_fresh_bounded_heap_and_one_command_runs_both_suites(self):
        """A case, or its fixture function, takes an allocator: Vec code is tested natively,
        each case in a store of its own with the default heap, which a case can exhaust without
        a trap. `grimoire test` then runs the declared executable tests too. What cannot be
        supplied is refused at the case's file and line, in the author's path, naming what can."""
        (self.project / "values.wy").write_text("""import vec

pub fn total(allocator: &Allocator, count: U64) -> U64:
    let Ok(made): Vec<U64> = Vec.filled(allocator, count, 2) else Err(code):
        return 0
    var sum: U64 = 0
    for item in &made:
        sum += *item
    made.close()
    return sum
""")
        (self.project / "tests/fixtures.wy").write_text(
            "type Heap:\n    allocator: Allocator\n\nfn owned(allocator: Allocator) -> Heap:\n    return Heap(move allocator)\n")
        entry = self.project / "test.wy"
        entry.write_text("""import "values.wy"
import "tests/fixtures.wy"
import testing

test fn lent(allocator: &Allocator) -> Result<(), Failure>:
    return testing.eq_u64(values.total(allocator, 4), 8)

// 20000 words are more than the three pages of a case's heap: an ordinary failure, no trap.
test fn bounded(allocator: &Allocator) -> Result<(), Failure>:
    return testing.eq_u64(values.total(allocator, 20000), 0)

// And the heap is this case's own: most of it is free again here.
test fn fresh(allocator: &Allocator) -> Result<(), Failure>:
    return testing.eq_u64(values.total(allocator, 2000), 4000)

using fixtures.owned
test fn through_a_fixture(allocator: &Allocator) -> Result<(), Failure>:
    return testing.eq_u64(values.total(allocator, 3), 6)
""")
        selftest = self.project / "selftest"
        selftest.mkdir()
        (selftest / "main.wy").write_text("fn main(console: Console<Write>):\n    console.print_int(7)\n")
        (selftest / "witchy.toml").write_text('[run]\nentry = "main.wy"\n[grants]\noutput = true\n')
        manifest = self.project / "witchy.toml"
        manifest.write_text(manifest.read_text() + '\n[tests]\ncase-deadline-ms = 250\n\n[[test]]\nname = "prints"\nproject = "selftest"\nstdout = "7\\n"\n')
        ran = self.cli()
        self.assertEqual(ran.stdout.splitlines(), ["pass test.wy::bounded  ", "pass test.wy::fresh  ", "pass test.wy::lent  ",
                                                   "pass test.wy::through_a_fixture  ", "ok prints"])
        report = json.loads(self.cli("--json").stdout)
        self.assertEqual(report["work"]["stores"], 4)
        self.assertEqual(self.cli("--list").stdout.splitlines()[0], "test.wy::bounded allocator: &Allocator")
        # The deadline of a case: from the manifest, or the command; out of range is refused.
        self.assertIn("the case deadline is a number of milliseconds from 25 to 4000", self.cli("--case-deadline-ms", "5", code=2).stderr)
        manifest.write_text(manifest.read_text().replace("case-deadline-ms = 250", "case-deadline-ms = 5000"))
        self.assertIn("the case deadline is a number of milliseconds from 25 to 4000", self.cli(code=2).stderr)
        self.cli("--case-deadline-ms", "400")
        # A failing declared test fails the one command, after the native cases ran.
        manifest.write_text(manifest.read_text().replace("case-deadline-ms = 5000", "case-deadline-ms = 250").replace('stdout = "7', 'stdout = "8'))
        failed = self.cli(code=1)
        self.assertIn("pass test.wy::lent", failed.stdout)
        self.assertIn("test prints failed", failed.stderr)
        entry.write_text(entry.read_text().replace("fn lent(allocator: &Allocator)", "fn lent(clock: &Clock)").replace("values.total(allocator, 4)", "8"))
        refusal = self.cli(code=2).stderr
        self.assertIn("paired-values/test.wy:5:9: W0401", refusal)
        self.assertIn("this case takes `clock: &Clock`, which the test host cannot supply", refusal)
        self.assertIn("`&Allocator` or, in a fixture function, `Allocator` (a fresh, bounded heap for each case)", refusal)
        self.assertNotIn("grimoire-tests-", refusal)

    def test_independent_stores_and_pre_entry_refusal(self):
        entry = self.project / "test.wy"
        entry.write_text("""import testing

fn helper(value: U64) -> U64:
    return 10 / value

test fn divide(value: U64) -> Result<(), Failure>:
    return testing.eq_u64(helper(value), 5, source_site("quotient"))
""")
        messages = [
            {"op": "list"},
            {"op": "run", "entries": [{"id": "missing", "case": "test.wy::divide"}]},
            {
                "op": "run",
                "entries": [
                    {"id": str(i), "case": "test.wy::divide", "args": [str(n)]}
                    for i, n in enumerate([2, 1, 0, 2])
                ],
            },
        ]
        rows = [
            json.loads(line)
            for line in self.cli(
                "--session", data="".join(json.dumps(m) + "\n" for m in messages)
            ).stdout.splitlines()
        ]
        self.assertEqual([case["id"] for case in rows[1]["cases"]], ["test.wy::divide"])
        self.assertEqual(
            (rows[2]["status"], rows[2]["entered"], rows[3]["stores"]),
            ("blocked", False, 0),
        )
        self.assertEqual(
            [r["status"] for r in rows[4:-1]], ["pass", "assertion", "trap", "pass"]
        )
        self.assertEqual(rows[5]["site"]["key"], "test.wy::divide@quotient")
        self.assertEqual((rows[-1]["compiles"], rows[-1]["stores"]), (1, 4))

    def test_release_build_excludes_unmarked_test_helpers(self):
        (self.project / "witchy.toml").write_text('[run]\nentry="main.wy"\n')
        (self.project / "tests/only.wy").write_text(
            "pub fn number() -> U64:\n    return 7\n"
        )
        (self.project / "main.wy").write_text(
            'import "tests/only.wy"\nfn main():\n    return\n'
        )
        result = subprocess.run(
            [ROOT / "grimoire", "build", self.project],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("test member", result.stderr)
        self.assertFalse((self.project / "target/main.wasm").exists())

    def test_test_dependency_does_not_authorize_a_production_import(self):
        (self.project / "witchy.toml").write_text('[run]\nentry="main.wy"\n')
        for name in ("ordinary", "fixture"):
            library = self.project.parent / name
            library.mkdir()
            (library / "lib.wy").write_text("pub fn number() -> U64:\n    return 7\n")
        (self.project / "main.packages").write_text("ordinary: ../ordinary/lib.wy\n")
        (self.project / "test.packages").write_text("fixture: ../fixture/lib.wy\n")
        (self.project / "main.policy").write_text("main.wy: *\n@ordinary: *\n")
        (self.project / "test.policy").write_text(
            "<standard>/: *\ntest.wy: *\n@fixture: *\n"
        )
        entry = self.project / "main.wy"
        entry.write_text(
            'import "@ordinary"\nfn main():\n    let value = ordinary.number()\n'
        )
        (self.project / "test.wy").write_text(
            'import "@fixture"\nimport testing\ntest fn seven() -> Result<(), Failure>:\n    return testing.eq_u64(fixture.number(), 7)\n'
        )
        self.assertEqual(
            json.loads(self.cli("--json").stdout)["outcomes"][0]["status"], "pass"
        )
        entry.write_text(entry.read_text().replace("ordinary", "fixture"))
        self.assertIn("fixture", self.cli(code=2).stderr)


if __name__ == "__main__":
    unittest.main()
