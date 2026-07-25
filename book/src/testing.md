# Testing

witchy has a built-in test runner. A test is a function named `test_*`; it
passes by returning and fails by aborting. A plain test runs with zero real
authority. It may be pure, use ordinary collaborators, or receive deterministic
fixture-backed capabilities from an external plan. An integration test may
instead receive real `Dir` or `Net` grants, but only through the explicit
integration tier. The `testing` module provides assertions and small ordinary
collaborators; it does not mint capabilities.

```witchy
import testing

fn double(n: Int) -> Int:
    n * 2

fn classify(n: Int) -> String:
    if n > 0: "positive" else if n == 0: "zero" else: "negative"

fn test_double():
    testing.assert_int_eq(double(21), 42)

fn test_classify():
    testing.assert_eq(classify(5), "positive")
    testing.assert_eq(classify(0), "zero")
    testing.assert(classify(0 - 3) == "negative", "negatives classify as negative")

trait Gateway:
    fn charge(self, cents: Int) -> String

type FakeGateway:
    canned: String

impl Gateway for FakeGateway:
    fn charge(self, cents: Int) -> String:
        self.canned

fn checkout(gateway: g, cents: Int) -> String where g: Gateway:
    gateway.charge(cents)

fn test_checkout_uses_the_gateway_result():
    let fake = FakeGateway(canned: "FAKE-OK")
    testing.assert_eq(checkout(fake, 500), "FAKE-OK")
```

Run them:

```sh
witchy test math.witchy
```

```text
running 3 test(s) in math.witchy
test math.test_double ... ok
test math.test_classify ... ok
test math.test_checkout_uses_the_gateway_result ... ok

test result: ok. 3 passed; 0 failed
```

Point `witchy test` at a directory and it runs every `test_*` across all the
`.witchy` files; a failure makes the command exit non-zero, so it drops straight
into CI.

## Plain tests have no real authority

Look at those test functions: none take a capability. That's not a restriction
the runner imposes arbitrarily: plain `witchy test` instantiates the compiled
test artifact with zero real host grants. A test therefore cannot accidentally
hit the network, write a file, inspect the environment, or depend on the real
clock. Effectful production functions may live beside the tests, but the runner
does not grant them authority merely because they were linked into a test.

Keeping effects at the edges leaves the program's core pure, so plain tests need
no host setup and cannot accidentally exercise real authority. Unused effectful
production functions are pruned from each synthesized test artifact; merely
linking a function that mentions `Dir`, `Fetch`, or `Exec` does not grant it.

## Stubs, fakes, strict mocks, and fixtures

Witchy uses these terms deliberately:

| Term | Meaning |
|---|---|
| Collaborator | Ordinary data, function, or trait value passed to domain code |
| Stub | A collaborator or fixture configured to return canned values |
| Fake | A deterministic implementation with useful behavior, such as an in-memory filesystem |
| Strict mock | An ordered fixture script that fails on an unexpected, missing, or differently attenuated call |
| Fixture capability | A sealed capability backed only by validated inert plan data |
| Integration grant | Real host authority explicitly supplied to an owned integration test |
| Browser provider | An explicitly enabled browser host provider, either real or fixture-backed |

Prefer an ordinary collaborator for a unit-sized seam. `FakeGateway` above is
cheap, typed, and follows exactly the same dispatch rules as production code.
Use a fixture capability when the provider contract itself matters: rights,
attenuation, call order, failure injection, filesystem state, origin checks, or
the proof that no undeclared operation occurred.

Witchy does not monkeypatch statically bound functions. There is no global mock
registry and no source-level interception syntax.

## Deterministic capability fixtures

A fixture run accepts one versioned JSON plan:

```json
{
  "version": 1,
  "console": {},
  "argv": ["Ada"]
}
```

The test asks for the roots it needs:

```witchy
capability TestRoot:
    console: Console
    args: List(String)

fn test_greeting(root: TestRoot):
    match root:
        TestRoot(console, args) ->
            console.print("hello, " + args.at(0))
```

Run the same plan against both local backends:

```sh
witchy test --fixtures fixtures.json --backend both fixture_suite.witchy
```

`both` is the default for fixture runs. It executes the interpreter and compiled
Wasm adapters against the same state machines, then compares result kind,
guest-visible output, ordered events, and transcript. Use `interpreter` or
`wasm` only when isolating a backend.

The plan can provide:

- scripted Console input and captured output;
- controlled Clock values;
- seeded or scripted Rand values;
- a named, attenuable Env map;
- an in-memory handle-confined Dir/File tree with rights and failures;
- origin-scoped scripted Fetch responses;
- an opaque SecretStore whose transcript never contains secret bytes;
- allowlisted scripted Exec results without spawning a process;
- argv launch input.

Raw `Net` is deliberately not fixture-backed. DNS, socket scheduling, packet
boundaries, TLS, and partial transport would amount to a second networking
stack; use `Fetch`, an ordinary collaborator, or an explicit integration test.
`File` and `Secret` are derived from fixture `Dir` and `SecretStore` handles
rather than minted as roots. VM is a zero-authority facility, not a fixture
family; its sequential reference behavior remains independently parity-tested.

Fixture capability records are assembled only for the selected package's test
module. The compiler flattens a concrete named-field capability record into its
declared fixture roots, constructs the sealed record at the authenticated test
boundary, and passes it to the test. Dependency tests cannot inherit that
construction privilege. Generic or recursive fixture aggregate records are
rejected before execution.

Plans are inert data. They cannot name a host path, ambient environment source,
socket, executable path, callback, descriptor, or native object. Unknown
versions and fields, duplicate JSON keys, malformed UTF-8, invalid paths,
oversized values, forged handles, and inconsistent scripts fail closed before
or during the bounded fixture session.

## Transcripts and CI output

Every fixture run produces a normalized transcript containing the seed, ordered
provider events, stdout, stderr, and result. `--format json` emits one stable
schema-2 document and includes each test's transcript:

```sh
witchy test --fixtures fixtures.json --format json fixture_suite.witchy
```

Passing output is captured unless `--show-output` is set. Failed fixture tests
show partial output automatically and retain it in JSON. Exit status `0` means
success, `1` means tests completed with failures, and `2` means usage, fixture,
or infrastructure failure. `--list` discovers without executing;
`--filter <text>` selects after discovery; `--seed <u64>` overrides a declared
Rand fixture reproducibly.

## Integration tests use explicit real grants

When a test must exercise a real filesystem or network boundary, declare the
capability in its signature and opt into the integration tier:

```witchy
import testing

fn test_reads_fixture(root: Dir[Read]):
    testing.assert_eq(root.read("fixture.txt"), "expected")
```

```sh
witchy test --integration --dir ./fixtures suite.witchy
```

`--dir` may be repeated for multiple `Dir` parameters. `--net <addr>` builds the
allowlist for a `Net` parameter. Omitting `--integration` or a required grant is
a failure with a diagnostic naming the missing flag; capability-parameterized
tests are never silently skipped. Currently the real-capability tier accepts
`Console`, `Dir`, and `Net`; other capability kinds remain unsupported until
they have an explicit CLI grant surface.

Authority follows the test entrypoint, not everything linked beside it. A
nullary test remains zero-grant even during an integration run, and an imported
library function cannot widen the synthesized test `main`. When a directory
run encounters tests from a manifest/lock-resolved dependency, those tests
always receive zero real grants even if the caller supplied `--dir` or `--net`
for the root package.

## Testing with collaborators

Code that talks to a service should depend on the behavior it needs, then accept
that collaborator as a parameter. A test supplies an ordinary type with a fake
implementation of the same trait. The `Gateway` and `FakeGateway` definitions
in the example above show the complete pattern: production code accepts the
trait-bounded value, while the test chooses the concrete fake and its canned
result.

Function parameters work the same way when a one-method trait would add no
clarity. The important part is the explicit injection seam: Witchy does not
monkeypatch or intercept a statically bound top-level function for tests. The
test and production program exercise the same dispatch rules.

## Constructing sealed domain data

The entry module run by `witchy test` may directly construct a foreign sealed
*data* type. This lets a test create malformed or boundary-case values that the
type's production constructors intentionally prevent. Production commands
remain strict, and imported dependency modules do not inherit this privilege.

This relaxation does not manufacture authority. Sealed host capabilities such
as `Dir`, `Net`, and `Clock` still cannot be constructed from Witchy source, and
plain tests still receive zero real host grants. Fixture handles are minted only
by the runner from a validated plan. The old `testing.mock_dir` constructor and
its separate native/interpreter filesystem backends were deleted; the shared
fixture plan is the only capability virtualization path.

## The assertions

The `testing` module gives you:

| Assertion | Aborts unless |
|---|---|
| `assert(cond, msg)` | `cond` is true |
| `assert_eq(got, want)` | the two strings are equal |
| `assert_ne(got, other)` | the two strings differ |
| `assert_int_eq(got, want)` | the two `Int`s are equal |
| `assert_value_eq(got, want)` | the two values are equal (`where a: PartialEq, a: Show`) |
| `assert_value_ne(got, other)` | the two values differ (`where a: PartialEq, a: Show`) |
| `fail_with(msg)` | (always — an unconditional failure) |

`assert_value_eq` / `assert_value_ne` are the idiomatic choice for records,
enums, and other typed values: they compare with `==` and render the mismatch
through `Show` for you, so you don't hand-stringify at the call site. Reach for
`assert_eq` when you already have strings. All of these assertions call the
`fail` primitive from the errors chapter, so a failing assertion is a
message-carrying abort. Because aborts are part of parity, a test behaves
the same whichever backend runs it.

## Beyond your own tests

Three project-level checks run alongside your tests:

- **`witchy parity`** (from the last chapter) is differential testing for the
  *language itself* — it catches either backend disagreeing with the other.
- **The independent conformance corpus** pins exact language results,
  rejections, and capability footprints without deriving its expectations from
  either backend. This matters because two implementations can agree on the
  same wrong answer. Its positive-control mutation deliberately preserves
  interpreter/Wasm agreement while changing a result, then proves the
  independently stated expectation still fails.
- The documentation you're reading is tested. Every Witchy block is extracted
  and checked. Complete examples execute through compiled Wasm in fresh opaque
  browser frames with explicit providers and derived CSP; runnable examples are
  also checked against committed output and backend parity.

Parity and conformance answer different questions: parity asks whether the two
backends agree, while conformance asks whether an observable result matches the
language contract. A language feature needs both when it changes runtime
semantics; parser, type-checker, and capability-policy changes also need exact
rejection or authority expectations at their owning boundary.
