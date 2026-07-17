# Testing

witchy has a built-in test runner. A test is a function named `test_*`; it
passes by returning and fails by aborting. Plain tests take no parameters and
run with zero real authority. An integration test may instead declare `Dir` or
`Net` parameters and receives them only under the explicit integration tier.
The `testing` module provides assertions that abort with a readable message.

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
running 2 test(s) in math.witchy
test math.test_double ... ok
test math.test_classify ... ok

test result: ok. 2 passed; 0 failed
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
no host setup and cannot accidentally exercise real authority.

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

This relaxation does not manufacture authority. Sealed capabilities such as
`Dir`, `Net`, and `Clock` still cannot be constructed by a test, and plain tests
still receive zero real host grants. `testing.mock_dir` is an explicit
authority-free in-memory backend and works in both plain and integration tiers;
other capability-shaped fakes still require their own mock backends. Until one
exists, inject a trait or function around the effectful edge and test the logic
behind it.

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

Two project-level testing ideas are worth knowing about, even though they're run
by the maintainers rather than written by you:

- **`witchy parity`** (from the last chapter) is differential testing for the
  *language itself* — it's how the backends are kept honest.
- The documentation you're reading is tested. Every witchy example in this book
  and the reference is extracted by the test suite and type-checked. Blocks
  classified as runnable are executed against the committed output oracle. If
  the language changed and an example here went stale, the build would fail. So
  what you've read is, quite literally, what the language does.

That's the book. You can write pure logic, push effects to a thin authorized
edge, see and gate exactly what a program can do, run untrusted code confined to
its declared footprint, share code without inheriting its authority, and trust
that what you tested is what runs. Go build something — and you'll know precisely
what it's allowed to do.
