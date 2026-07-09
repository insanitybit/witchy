# BUG-509: Async docs still say `var` cannot cross `await`

Severity: LOW
Status: FIXED
Verified: 2026-07-09 fixed on master 040ce13b
Component: `spec/language.md`, book iterator/async tours, async/await language surface

## Resolution

`spec/language.md` and `book/src/tour-iterators.md` now describe the current
RFC-0059-style lowering: async functions lower to state-machine segments, live
locals are threaded through segment parameters, and mutable locals may cross an
`await` in supported positions. The same prose names the remaining restriction:
`await` is still rejected in branch conditions, loop conditions, and match
scrutinees.

The implementation already supported this behavior; this row tracked stale docs.

## Problem

Historical problem: some release-facing language docs taught the pre-RFC-0059
async limitation:
an `async fn` cannot carry a mutable `var` local across an `await` because the
rest of the function is captured by value in a continuation closure.

That was no longer the shipped behavior. The current async lowering is a segment
state machine that threads live locals through segment parameters. The async tour
now demonstrates this correctly, but `spec/language.md` and the iterator tour
still repeat the old limitation.

This made the language look less powerful than it is, and worse, it made two
current docs disagree with each other.

## Reproduction

`scratch/repro-async-var-crosses-await.witchy`:

```witchy
import chan

async fn main(console: Console):
    var i = 0
    while i < 3:
        chan.yield_now().await
        i = i + 1
    print(console, "${i}")
```

Verified:

```text
$ cargo run --quiet -- check scratch/repro-async-var-crosses-await.witchy
scratch/repro-async-var-crosses-await.witchy: ok
$ cargo run --quiet -- scratch/repro-async-var-crosses-await.witchy
3
```

## Evidence

- `crates/witchy-syntax/src/async_lower.rs:39-44` says the current lowering lets
  a mutable `var` local cross an `await`, allows `await` inside a `while`, and
  lets `for await` fold into an accumulator.
- `crates/witchy-syntax/src/async_lower.rs:117-122` models carried locals as
  continuation-segment parameters, preserving mutability as local state.
- `crates/witchy-syntax/src/async_lower.rs:574-576` documents `while` bodies
  that await as recursive segment loops threading counter/accumulator state.
- `book/src/tour-async.md:176-209` correctly documents and demonstrates a
  `while` loop and a `for await` fold mutating `var` locals across awaits.
- `spec/language.md:518-521` still says `await` lowers to a continuation closure
  and a `var` declared before an `await` cannot be mutated after it.
- `book/src/tour-iterators.md:158-163` still contrasts generators with async by
  saying an `async fn` cannot carry a `var` across an `await`.

## Why this matters

`var` across `await` is not a corner case. It is the natural way to write
producer counters, retry loops, stream folds, and protocol state machines. If
the public docs say users must recurse or route state through channels, the
async surface looks much more awkward and half-finished than it is.

This is distinct from BUG-504. BUG-504 tracks docs that still say `await` inside
a `while` body is unsupported. This bug tracks the stale state-carrying rule for
mutable locals across `await`.

## Expected fix

Update current-state docs to describe the RFC-0059 state-machine lowering:

- `var` locals may cross an `await` when the await is in a supported position;
- `await` in `while`/`for` bodies and `for await` folds can thread mutable state;
- `await` remains unsupported in conditions and match scrutinees, if that
  limitation is still current.

Historical RFC text can keep the old limitation only if it is clearly marked as
historical rather than current language guidance.

## Acceptance

- `spec/language.md` no longer says `await` lowers the rest of the function into
  a closure that forbids mutating a pre-await `var`.
- `book/src/tour-iterators.md` no longer claims async cannot carry a `var` across
  `await`.
- The async tour and spec agree on the supported `var`-across-`await` behavior
  and on the remaining unsupported await positions.
