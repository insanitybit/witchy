# Testing

witchy has a built-in test runner. A test is a function named `test_*` that
takes no parameters; it passes by returning and fails by aborting. The `testing`
module provides assertions that abort with a readable message.

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

## Tests are capability-free

Look at those test functions: none take a capability. That's not a restriction
the runner imposes arbitrarily — it's the natural consequence of the model. A
test exercises *logic*, and in witchy the logic worth testing is the pure core,
which needs no authority. A test suite therefore **provably has no effects**: it
can't accidentally hit the network, write a file, or depend on the clock. You
can run anyone's witchy tests without wondering what they'll touch.

This is the payoff of the structure the project chapter pushed: when you keep
effects at the edges, the middle — the part you test — is pure, and testing it is
trivial and safe.

## The assertions

The `testing` module gives you:

| Assertion | Aborts unless |
|---|---|
| `assert(cond, msg)` | `cond` is true |
| `assert_eq(got, want)` | the two strings are equal |
| `assert_ne(got, other)` | the two strings differ |
| `assert_int_eq(got, want)` | the two `Int`s are equal |
| `fail_with(msg)` | (always — an unconditional failure) |

Render values to strings at the call site (`"${x}"`) so the
failure message stays readable. Under the hood these all call the `fail`
primitive you met in the errors chapter, so a failing assertion is just a loud,
message-carrying abort — and because aborts are part of parity, a test behaves
the same whichever backend runs it.

## Beyond your own tests

Two project-level testing ideas are worth knowing about, even though they're run
by the maintainers rather than written by you:

- **`witchy parity`** (from the last chapter) is differential testing for the
  *language itself* — it's how the backends are kept honest.
- The documentation you're reading is tested. Every witchy example in this book
  and the reference is extracted by the test suite, type-checked, and — when it's
  a `Console`-only program — run on both backends with its output verified. If
  the language changed and an example here went stale, the build would fail. So
  what you've read is, quite literally, what the language does.

That's the book. You can write pure logic, push effects to a thin authorized
edge, see and gate exactly what a program can do, run untrusted code confined to
its declared footprint, share code without inheriting its authority, and trust
that what you tested is what runs. Go build something — and you'll know precisely
what it's allowed to do.
