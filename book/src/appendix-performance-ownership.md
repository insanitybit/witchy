# Performance: syntax and ownership contracts

Ownership conventions are language contracts first and optimization inputs
second. They preserve Witchy's value semantics at calls and give the compiler
facts it can use without a tracing garbage collector.

This is the main performance chapter. Choose the source contract here first;
use the [compiler-switch chapter](appendix-performance-knobs.md) only after a
real workload gives you a reason to isolate one backend pass.

| Convention | Source meaning | Optimization opportunity |
|---|---|---|
| `fn f(xs: T)` | owned value with an independent callee view | Safe baseline; the callee may copy or consume its copy |
| `fn f(let xs: T)` | non-escaping read-only call input | Share the input during the call without making a new owner |
| `fn f(own xs: T)` | callee consumes the caller's value | Reuse the incoming storage and carry its ownership token |
| `fn f(var xs: T)` | callee returns a value to the caller's slot | Update the caller slot directly when the place is proven safe |
| `move xs` | consume this binding at one call site | Transfer the existing owner instead of copying it |
| `unique T` | sole owner of mutable logical storage | Permit in-place mutation and extraction |
| `local unique T` | unique only inside the current activation | Use temporary storage while preventing an escaping reference |
| `frozen T` | deeply immutable owned value | Share reads, while rejecting mutation through the value |

The default convention is intentionally useful. It means callers do not need
to understand representation or lifetimes, and the compiler still performs
proof-driven optimization inside the function.

## Syntax is the optimization interface

The useful question is not “which flag makes this faster?” It is “what is true
about this value at this boundary?” The answer determines which transformation
is sound:

| Source fact | What the compiler may do |
|---|---|
| A `let` helper only reads during the call | Borrow the input without creating a new owner |
| A `var` function returns a whole local to its caller slot | Avoid a scratch copy and use direct write-back |
| An `own` parameter is consumed with `move` | Carry the existing buffer across the call |
| A value is `unique` | Mutate or extract in place while the proof holds |
| A value is `frozen` | Share reads without opening a mutable path |
| A value is `local unique` | Reclaim or optimize temporary state without allowing it to escape |
| A type is `packed` | Use a fixed inline layout where every boundary agrees |
| A block is `region:` | Reclaim its temporary allocations together |

The compiler can still optimize code that uses none of these forms. The forms
matter when they expose a fact that ordinary value syntax cannot safely assume.
They are contracts to write because they are true, not annotations to sprinkle
until a benchmark moves.

## `let`: a read-only call boundary

Use `let` when a helper only inspects its input and does not retain it:

```text
fn checksum(let bytes: List(Int)) -> Int:
    // read and reduce bytes; do not return or store it
```

The checker rejects a `let` value that escapes through a result, aggregate,
closure, task, channel, or other boundary. That restriction is what lets the
compiled backend borrow the input temporarily. `let` does not mean that the
caller may mutate the value during the call, and it does not create an
explicit `&'a T` reference. For those relations, use [Opt-mode References and
Lifetimes](opt-mode-references.md).

## `own` and `move`: one buffer across a call

The useful shape is a consume-and-return pipeline:

```witchy
fn append(own xs: List(Int), value: Int) -> List(Int):
    var result = move xs
    result.push(value)
    result

fn main(console: Console):
    var values = [1, 2]
    values = append(move values, 3)
    console.print("${values.length()}")
```

```text
3
```

`move` is local and explicit. After `append(move values, 3)`, the old `values`
binding cannot be used until the returned value is assigned. If another live
binding or reference could observe the old storage, the compiler must take a
copy or reject the opt-mode program rather than reusing it.

## `var`: write-back without confusing the referent

`var` is about the caller's slot, not about a hidden mutable reference:

```witchy
fn trim(var text: String) -> Nil:
    text = text.trim()

fn main(console: Console):
    var text = "  witchy  "
    trim(text)
    console.print(text)
```

The callee receives a value and its final value is written back. A `var` call
can use direct storage when the argument is a whole, disjoint local. A field,
live loan, alias, or uncertain call preserves staged write-back. See
[`direct-storage-var`](appendix-performance-knobs.md#direct-storage-var-direct-var-write-back)
for the exact proof shape.

## `unique`, `frozen`, and `local unique`

These qualifiers describe owned storage, not temporary reference syntax:

```text
unique List(Int)       // the sole owned storage may be updated in place
local unique Buffer    // unique here; cannot escape this activation
frozen Config          // owned and deeply immutable
&'a mut Buffer         // an exclusive reference for lifetime 'a
```

An active `&'a mut T` loan temporarily makes its owner unavailable to unrelated
updates. Conversely, an owned `unique T` value can open a mutable reference
only when the borrow checker can prove the owner and projection are exclusive.
`unique` is not a synonym for `&mut`, and `var` is not a synonym for either.

Use `local unique` for scratch state that must be reclaimed or optimized inside
one call but must not be returned, stored in a task, or captured by an escaping
closure. Use `frozen` when sharing reads is the contract and mutation would be
a bug, not merely a missed optimization.

## Aliases are local proof losses

An alias should disable reuse only where it is created:

```text
var xs = [1, 2, 3]
let snapshot = xs
xs.push(4)       // copy-on-write: snapshot must retain [1, 2, 3]
```

After `snapshot` is dead, a later independent accumulation can become unique
again. A read-only helper summarized as `let` can remain in the chain. An
unknown function value, host call, trait object, closure capture, or container
insertion is conservatively treated as an escape until its summary proves
otherwise.

## Functional-in-place state kernels

In `mode opt`, a narrow recursive shape can be lowered as a loop while carrying
one owned state record. The shape requires a direct self-call, an `own unique`
state parameter, scalar auxiliary arguments, explicit owner return on every
base path, and the recursive call as the final expression:

```text
mode opt

fn count(own unique state: Counter, remaining: Int) -> Counter:
    if remaining == 0:
        state
    else:
        state.value = state.value + 1
        count(move state, remaining - 1)
```

This is a constrained lowering proof, not a new `fip` keyword. If the state is
aliased, the recursive call is indirect, a field escapes, or a path fails to
return the owner, the compiler retains the ordinary representation or reports
the opt-mode error. Verify the claim with allocator and reclaimer counters,
not with recursion depth alone.

## Explicit references are the lower-level escape hatch

Normal mode remains reference-free. In opt mode, use `&'a T` and `&'a mut T`
when the access relation itself must cross a call, be projected, or be stored
in a relation-preserving aggregate:

```text
mode opt

fn first(let text: &'a String) -> &'a String:
    text
```

`let` qualifies the reference handle in the current executable opt-mode
surface; it does not change `&'a String` into the retired `let('a) String`
syntax. The full lifetime and escape model is documented in [Opt-mode
References and Lifetimes](opt-mode-references.md).

Here is the smallest complete borrowed-view program. The dereference at the
print call is explicit: the console receives an owned `String` value, while
`view` remains a reference handle:

```witchy
mode opt

fn first(let text: &'a String) -> &'a String:
    text

fn main(console: Console):
    var s = "borrowed, not copied"
    let view = first(&s)
    console.print(*view)
    s = "now the view is done, so the owner is free again"
    console.print(s)
```

```text
borrowed, not copied
now the view is done, so the owner is free again
```

When the owner must be used before the view's last use, materialize deliberately
with `.owned()`:

```witchy
mode opt

import borrow

fn first(let text: &'a String) -> &'a String:
    text

fn main(console: Console):
    var s = "borrowed"
    let kept = first(&s).owned()
    s = "the view is materialized, so the owner is free right away"
    console.print(kept)
    console.print(s)
```

```text
borrowed
the view is materialized, so the owner is free right away
```
