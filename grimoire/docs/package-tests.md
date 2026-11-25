# Package tests

Run `grimoire test .` in a package with a `test.wy`. Import other test members from `tests/`. Only a `test fn` is a case. An ordinary helper stays private unless exported. A test member can call a private function in an explicitly imported module of its resolved package. Sealing, borrowing, capability ceilings and proofs apply to the test build. The production entry is checked separately when declared.

```sh
grimoire test examples/package-tests --list
grimoire test examples/package-tests --filter '*::adds' --json
```

A case can select a fixture with `using` and a local or import-qualified function name. The compiler reads the case’s checked parameter labels, types and borrow modes. The provider returns a record with a field for each parameter. Owned fields can be lent to the case and borrowed fields can be passed through. When a case takes ownership, the adapter opens the fixture once and moves its fields into the call. Normal sealing and loan checks apply. An incompatible field fails at the case declaration during compilation.

```witchy
import "values.wy"
import "tests/fixtures.wy"
import testing

using fixtures.example
test fn adds(pair: &Operands, expected: U64) -> Result<(), Failure>:
    return testing.eq_u64(values.sum((*pair).left, (*pair).right), expected)
```

The example provider constructs `Example(Operands(19, 23), 42)` entirely in Witchy. It takes no external inputs, so the default runner binds this case with an empty grant. Moving the case leaves the provider and its values in place. [Runnable case](../examples/package-tests/test.wy) · [Fixture types and provider](../examples/package-tests/tests/fixtures.wy)

## What a case may take

A case's parameters, or those of the fixture function it names with `using`, come from the test host. These are all it can supply:

| Parameter | What the case gets |
| --- | --- |
| `&Allocator` (in a fixture function also `Allocator`, to keep in the record it returns) | a heap of the case's own: every case runs in a fresh store with the default three-page heap and 64 allocation slots, so it starts empty, a case cannot exhaust another's, and running out is an ordinary `Err`, not a trap |
| `U64` | a word the runner passes (`--word`, `offer_word`, a session's `args`) |
| `&Console<Read>` | bytes the runner passes (`--bytes`, `offer_bytes`) |
| `&Dir<Read>`, `&Dir<Write>` | a directory the runner binds (`--read-dir`, `--fixture`, `offer_directory`, ...) |

So code that uses `Vec`, `String` or `HashMap` is tested natively:

```witchy
test fn totals(allocator: &Allocator) -> Result<(), Failure>:
    return testing.eq_u64(notes.total(allocator, 4), 8)
```

Anything else (a `Clock`, a `Listener`, a record of your own) is refused when the suite is checked, at the case's file and line, naming this list: `test.wy:7:9: W0401 ...: this case takes `clock: &Clock`, which the test host cannot supply. ...`. Build such a value in a fixture function from the types above. A fixture function must not be named after a standard module (`memory`, `vec`, ...).

Diagnostics name your files (`tests/samples.wy:12:5`), not the staged copy the suite is checked from.

## One command, and the time a case gets

`grimoire test .` runs the native cases and then each executable the manifest declares with `[[test]]` (name, project, exact stdout, and optionally `stdin`: the text given as its standard input when its grant document grants `[input] ... = { stdin = true }`; empty otherwise), and fails if either fails. With a selecting flag (`--case`, `--filter`, `--list`, `--json`, ...) it runs the native cases only; `--projects` runs only the declared executables.

A case may run for 100 ms. That is the time the host measures inside the case: the host's own start and the module's compilation, which the first case of a run triggers, are not charged to it. Set another limit, from 25 to 4000 ms, for a run with `--case-deadline-ms N` or for the package in its manifest:

```toml
[tests]
case-deadline-ms = 250
```

## Supplying resources

The default runner binds empty, scalar or explicitly provided slots and reports every selected case. An optional `main(tests: Tests, grants: TestGrants)` can offer named fixtures to selected descriptors. Collection opens no resources. Each bind checks all required slots against the compiler catalog. Running the successful binding consumes its ticket. The supervisor reports an omitted invocation as unrun.

```witchy
for case in tests.collect():
    grants.offer_directory(case, "left", "roles", "publisher")
    grants.offer_directory(case, "right", "roles", "reviewer")
    match grants.bind(case):
        Bound(invocation):
            invocation.run()
        Blocked(slot):
            continue
```

Here the operator supplies `--read-root roles=/absolute/tree`. Both subdirectories must be inside that root. Each slot keeps the selected object's filesystem identity and right until the host opens it. A missing root, a symlink or a different right blocks the invocation. An explicit `--read-dir 'test.wy::roles:left=/absolute/object'` pins one case slot. The write variants authorize write operations. An unselected case opens no objects.

A suite can use `grants.has_root("roles")` to choose an offered root or an authored synthetic alternative. The query checks the name without opening the directory. Choosing the real root still requires its right and object identity to pass binding.

For disposable fixtures use `offer_fixture(case, "left", "role", "01")`. For public data use `offer_bytes` or `offer_word`. Offers match an external parameter’s label and transport type. The command-line equivalents are `--fixture CASE:SLOT=FILENAME:HEX`, `--bytes CASE:SLOT=HEX` and `--word CASE:SLOT=DECIMAL`. Explicit session inputs override scalar or byte defaults. Ordinary user-defined record types stay inside the guest and use the usual type and ownership rules.

## Sessions, properties and proofs

`grimoire test . --session` sends a version-1 JSON line with an artifact ID. Send `{"op":"list"}` to read the selected cases. Each case lists the parameters that the test receives and the external slots that binding requires. Run a batch by sending one line such as the following.

```json
{"op":"run","entries":[{"id":"one","case":"test.wy::adds"},{"id":"again","case":"test.wy::adds"}]}
```

Exit 0 means the selected run completed successfully. Assertions and target traps exit 1. Blocked, unrun, interrupted, admission and budget failures exit 2.

Each response identifies the request and case. A final `done` line counts outcomes and gives cumulative Wasmtime compilations and stores. Reuse that process for another batch. Use `--artifact ID` to pin a separate session. Paths may be used for intentional case pins, glob selection and diagnostics. The selected suite can schedule up to 64 requests per batch. Each dispatched input starts in a fresh store. The default suite budget is five seconds including admission and cleanup. An ordinary invocation has a 100 ms limit (above), metered fuel and bounded linear memory. List and proof modes execute zero guests.

Import `prop` for deterministic trials, record derives, custom strategies and shrinking in Witchy. Use `--property CASE --seed 42 --trials 32 --save-replay failure.json` for isolated U64 trials and `--replay failure.json` for exact replay under the pinned artifact. The optional `--coverage` session returns bounded per-input guest edge IDs and a map for each module. Coverage requires public hermetic bindings. A working byte-driven client ships beside the Coven decoder.

`--proof DECLARATION` selects obligations from the ordinary checked inventory. For an isolated ordinary consumer under `tests/reject/`, pass `--reject tests/reject/upper.wy --expect W0402@2:13 --lemma upper`. A matching expected refusal publishes no artifact and executes no guest. The failure of an unrelated obligation or a missing checker cannot satisfy this expectation.

Authenticated single-source dependency releases currently contain production source only. Selecting one with `--dependency NAME` reports that its test suite is unavailable. A project containing only legacy `[[test]]` entries still runs them with `grimoire test .`. Select those entries explicitly with `--projects` when a native suite is present.

For local dependencies, a test entry can add package roots in `test.packages` under the reviewed `test.policy`. The production entry's package list and policy still govern ordinary source. For registry dependencies the supervisor uses Grimoire's frozen, authenticated lock resolution.

## The `testing` library

`import testing` gives a test what it returns. Every check takes the call's own source site by default, so a failure names its line.

| Function | Passes when | `reason` | What `grimoire test` prints |
| --- | --- | --- | --- |
| `testing.eq_u64(actual: U64, expected: U64)` | equal | 1 `u64_mismatch` | `eq_u64: got 42, expected 43` |
| `testing.eq_bool(actual: Bool, expected: Bool)` | equal | 2 `bool_mismatch` | `eq_bool: got true, expected false` |
| `testing.ok(condition: Bool)` | true | 3 `condition` | `ok: the condition was false` |
| `testing.eq_i64(actual: I64, expected: I64)` | equal | 4 `i64_mismatch` | `eq_i64: got -3, expected 5` |
| `testing.eq_bytes(actual: &[U8], expected: &[U8])` | same bytes | 5 `byte_mismatch`, 6 `length_mismatch` | `eq_bytes: the byte at index 2 is not the expected 65 (0x41)`; `eq_bytes: got 3 bytes, expected 6; ...` |
| `testing.set_up(made: Result<T, U64>) -> Result<T, Failure>` | `Ok` (and gives the value: `let items = testing.set_up(Vec.new(allocator, 4))?`) | 7 `set_up_failed` | `set_up: the step answered Err(1)` |

Each check returns `Result<(), Failure>` (but `set_up`). `Failure` has four `U64` fields: `reason` (which check, as above), `site` (where it was called), `actual` and `expected`. A failed case prints one line: `assertion test.wy::doubles u64_mismatch {... 'line': 5, 'column': 12 ...} eq_u64: got 42, expected 43`; with `--json` the same is `reason`, `site`, `actual`, `expected` and `detail`. Checks compose with `?` in a test that returns `Result<(), Failure>`.
