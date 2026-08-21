---
rfc: 0138
title: "Callable delegation, pure functions, and single-use closures"
status: proposed
created: 2026-08-21
superseded-by:
tracking: "Design proposal. No syntax or behavior in this RFC is implemented until its acceptance rows carry executable evidence."
related:
  - "[0125](0125-core-language-contract.md) (functions and function values)"
  - "[0126](0126-capability-effects-contract.md) (explicit authority and effects)"
  - "[0127](0127-ownership-and-opt-mode.md) and [0114](0114-must-consume-obligations.md) (own, move, and disposition obligations)"
  - "[0005](0005-unforgeable-capabilities.md) (reference-safe capability-bearing closure environments)"
---

# RFC-0138: Callable delegation, pure functions, and single-use closures

## Summary

An ordinary closure is an opaque delegated capability. A caller that hands a
closure to another function deliberately grants the right to invoke that exact
behavior; the receiver does not thereby receive, possess, or gain access to the
capabilities captured inside the closure.

This RFC therefore rejects mandatory captured-capability rows on ordinary
function types. A callee writes the callable interface it needs, not the hidden
authority its caller may use to implement that interface.

Two optional callable contracts express the properties APIs actually need:

- `pure fn(A) -> B` may perform no externally observable authority effect;
  and
- `once fn(A) -> B` is affine and invocation consumes it, so it can be called
  at most once.

`own` remains a parameter convention. It transfers ownership of a value to the
callee but does not limit how many times an ordinary callable may be invoked.
Purity, invocation cardinality, ownership transfer, and must-consume disposition
are distinct axes.

## Motivation

### A closure is already an object capability

Consider a caller that holds `Console` and constructs this closure:

```witchy-static
let done = fn():
    console.print("done")

run_plugin(done)
```

`run_plugin` does not receive `Console`. It cannot read stdin, print an arbitrary
message, narrow or re-export the console handle, or recover the closure's
environment. It receives one smaller authority: the right to request that the
fixed action `done` run whenever it invokes the callable.

That is ordinary object-capability delegation. The callable's interface is the
authority. Its captured implementation is encapsulated.

Annotating `done` as `captures {Console}` would be true as an implementation
fact and misleading as an authority fact. It would describe the broader object
used to implement the closure rather than the narrower operation delegated to
the receiver.

### The current purity claim is too strong

The capability guide currently says that a function with no capability
parameters provably has no effects. That is true only when the function also
has no authority-bearing values available through its lexical environment,
arguments, aggregates, or callbacks.

```witchy-static
fn invoke(callback: fn(String) -> Nil):
    callback("record this")

fn main(console: Console):
    let log = fn(message: String):
        console.print(message)
    invoke(log)
```

`invoke` has no `Console` parameter, but the caller deliberately hands it a
logging operation. Nothing was forged and no root grant widened. The callback
is delegated authority.

The corrected rule is:

> Code can exercise only authority it receives directly or transitively
> through values. A callable is one such value, and invoking it exercises the
> behavior its creator delegated.

This preserves the capability model while removing the incorrect inference
that every unqualified `fn(...)` value is pure.

### APIs need behavioral constraints, not capture disclosure

Some APIs genuinely require a stronger callable contract:

- collection transforms, deterministic build functions, and untrusted numeric
  plugins may require an effect-free callback;
- completion handlers, transaction continuations, and replay-sensitive actions
  may permit one invocation only; and
- an API that stores a callback beyond the call must take ownership of it.

Those requirements are about behavior, cardinality, and ownership. Requiring a
callee to enumerate its caller's hidden captured capabilities is both inverted
and needlessly coupled to the caller's implementation.

## Terminology

This RFC uses four terms precisely:

- **captured authority** is an implementation detail stored in a closure's
  environment;
- **delegated authority** is the behavior exposed by the callable interface;
- **latent effect** is an externally observable authority effect that may occur
  when the callable is invoked; and
- **root authority** is a capability supplied by a host entrypoint grant.

A receiver of `fn() -> Nil` has delegated authority to invoke that function. It
does not possess every root capability used behind the function. A package
footprint must not report those hidden roots as capabilities demanded by the
receiver's function merely because some caller may choose such an
implementation.

## Scope and non-goals

This RFC applies `pure` and `once` to ordinary synchronous function values and
lambdas. It does not add qualified async functions, generator values, isolated
worker transport, native callbacks, capture mutation, or an `FnMut` analogue.
Those boundaries retain their existing contracts.

`pure` is not a claim of termination, bounded resource use, constant-time
execution, absence of traps, or suitability for untrusted execution. `once`
limits calls to the outer callable; it does not limit how many effects the
callable's creator placed inside that one invocation.

## Design

### Ordinary `fn` remains opaque and potentially effectful

The existing callable spelling keeps its source form:

```witchy-static
fn(A) -> B
```

It means:

- the value may be a named function or a closure;
- its environment is opaque to the receiver;
- invoking it may perform effects authorized by its creator;
- it is reusable under ordinary value semantics; and
- passing it is deliberate delegation of its callable interface.

The type does not list captured capability kinds. A higher-order function may
accept, call, store, return, or compose such a value according to ordinary
ownership and escape rules.

A dependency update that begins invoking an already-delegated callback is a
semantic behavior change, not a capability-footprint widening. Capability diffs
cannot and should not pretend to replace review of arbitrary behavior changes.

### `pure fn`: checked effect-free invocation

The new callable qualifier `pure` is accepted on named function declarations,
lambda expressions, and function types:

```witchy-static
pure fn double(value: Int) -> Int:
    value * 2

let offset = 3
let add: pure fn(Int) -> Int = pure fn(value: Int):
    value + offset

pure fn apply(transform: pure fn(Int) -> Int, value: Int) -> Int:
    transform(value)
```

Calling a `pure fn` may compute, allocate ordinary managed values, use local
`var` bindings, return `Result`/`Option`, or terminate with the language's
ordinary trap semantics. It may not produce an externally observable authority
effect.

The checker accepts a `pure fn` body only when all of these hold:

1. it invokes no host-capability operation;
2. it invokes only callables whose type is also `pure fn`;
3. it captures no capability-bearing value, opaque ordinary callable, or other
   value whose invocation/use may exercise hidden authority;
4. it performs no `var` write-back through a parameter; and
5. it performs no task, channel, worker, dynamic-call, or other language
   operation classified effectful;
6. every referenced standard-library or intrinsic operation is classified
   pure by the same dependency-bottom effect catalog used by the checker.

Capturing immutable ordinary data is allowed. Local mutation is allowed because
it is not observable outside the call. Nontermination and traps are not modeled
as authority effects; `pure` means effect-free, not total.

The checker does not infer a public purity promise from an ordinary declaration.
`pure` is an explicit API contract and is checked at its declaration. A named
`pure fn` or pure lambda may widen to ordinary `fn`; an ordinary callable cannot
narrow to `pure fn` merely because one caller believes it harmless.

This conservative first cut may reject useful authority-preserving operations
such as rearranging capability values without exercising them. A later RFC may
relax that boundary after a concrete motivating API. The first contract favors
an obvious guarantee over an effect system with subtle exceptions.

### `once fn`: affine, consuming invocation

The new callable qualifier `once` describes invocation cardinality:

```witchy-static
fn complete(own callback: once fn(Result(String, Error)) -> Nil, result: Result(String, Error)):
    callback(result)
```

A `once fn` value is affine:

- it may be moved or passed to an `own` parameter;
- it may not be implicitly copied;
- invoking it consumes the callable on the attempted call edge;
- a second invocation or use after invocation is a check-time error; and
- it may be dropped unused; exact disposition comes from an enclosing nominal
  must-consume protocol.

Consumption happens when invocation is attempted, matching the existing rule
for an `own` call. A returned `Err`, propagated `?`, or trap does not restore the
callable.

`once` is a function-value and lambda qualifier, not a named-function
declaration qualifier. A top-level function is reusable code and each reference
to it may create a new callable value. An API that needs one invocation wraps
that named function in a fresh `once fn` lambda, as shown under Type relations.

A parameter that intends to invoke a once callable normally spells `own`. A
default parameter may receive one only from an explicit `move` argument; an
explicit `let` parameter borrows it and therefore cannot invoke it, because the
call would consume the caller's value. These are the existing convention rules
applied to the new affine callable type, not a second transfer mechanism.

A `once fn` lambda owns its environment. Copyable, droppable captures follow
ordinary capture-by-value semantics. The first cut does **not** permit an
ordinary or once closure to capture a live affine or must-consume value. The
function type hides its environment, so such a capture would erase an ownership
or disposition fact at the next opaque callable boundary.

This restriction is load-bearing. Moving a `must` transaction into a closure
and then letting a callee silently discard the closure would violate RFC-0114
even if the closure body would consume the transaction *if invoked*. The current
contextual exception that permits a lambda passed directly to an `own fn(...)`
parameter to transfer must-consume captures must therefore be removed or
replaced by a future callable type that exposes the obligation. `own` alone
cannot prove that the callee invokes the closure.

The ordinary `once fn` contract is **at most once**, not exactly once. It may be
dropped because its environment contains only droppable values. An API that
requires eventual invocation wraps it in an existing nominal must-consume
protocol:

```witchy-static
must sealed type Completion:
    Completion(once fn(Result(String, Error)) -> Nil)
```

The defining module exposes consuming operations such as `finish` and, if the
protocol permits cancellation, `cancel`. This keeps invocation cardinality
(`once`) separate from disposition (`must`).

An API that needs to combine a callback with a must-consume resource keeps the
resource visible in that nominal protocol rather than hiding it in the closure
environment:

```witchy-static
must sealed type TransactionCompletion:
    TransactionCompletion(Transaction, once fn(own Transaction) -> Nil)

fn finish(own completion: TransactionCompletion):
    match completion:
        TransactionCompletion(transaction, callback) -> callback(transaction)
```

The wrapper's declared `must` obligation survives every opaque boundary, and
the callback's `own` parameter makes the resource transfer explicit. A future
RFC may introduce a callable environment-obligation parameter if this pattern
proves too cumbersome; captured capability *names* are neither necessary nor
sufficient for that ownership problem.

### `own` transfers; it does not mean once

The existing parameter convention keeps its meaning:

```witchy-static
fn retain(own callback: fn(Event) -> Nil):
    // The caller's binding was consumed. This function owns a reusable callback.
    ...
```

`own callback: fn(...)` transfers the caller's value and prevents later use of
that caller binding. Inside `retain`, the ordinary callback remains reusable and
may be invoked more than once.

For a single-use callback, the two contracts compose:

```witchy-static
fn retain_one(own callback: once fn(Event) -> Nil):
    ...
```

`own` answers **who owns the callable after this call**. `once` answers **how
many invocation attempts the callable permits**. Neither implies the
other.

### The five axes are orthogonal

| Axis | Source contract | Meaning |
|---|---|---|
| Delegated behavior | `fn(A) -> B` | Receiver may invoke this opaque behavior |
| Effect constraint | `pure fn(A) -> B` | Invocation performs no authority effect |
| Invocation cardinality | `once fn(A) -> B` | Invocation consumes; at most one call |
| Ownership transfer | `own callback: T` | Callee consumes the caller's binding |
| Disposition obligation | `must type Wrapper` | Value must be consumed or transferred on every path |

`pure` and `once` may compose. The formatter's canonical order is:

```witchy-static
pure once fn(A) -> B
```

That value is effect-free and single-use. It remains droppable unless enclosed
in a nominal must-consume value.

### Type relations

Callable subtyping/coercion follows these rules:

- `pure fn(A) -> B` may widen to `fn(A) -> B`;
- `pure once fn(A) -> B` may widen to `once fn(A) -> B`;
- `once fn(A) -> B` cannot widen to reusable `fn(A) -> B`;
- reusable `fn(A) -> B` cannot narrow to `once fn(A) -> B`, because copying may
  already have occurred before the ascription;
- ordinary `fn(A) -> B` cannot narrow to `pure fn(A) -> B`; and
- all existing parameter conventions, references, lifetime relations, generic
  bounds, and concrete runtime kinds remain part of callable identity.

A named ordinary function may be explicitly adapted to `once fn` by a `once`
lambda that invokes it. This creates a fresh affine wrapper rather than
reclassifying an already-copyable value:

```witchy-static
let one: once fn(Int) -> Int = once fn(value: Int):
    reusable(value)
```

### Generic code is generic over behavior, not captured capability names

Ordinary higher-order code remains unchanged:

```witchy-static
fn invoke(callback: fn(A) -> B, value: A) -> B:
    callback(value)
```

It accepts any delegated implementation matching the callable interface. The
callee neither declares nor learns the caller's capture set.

Generic code that requires effect-free behavior names `pure fn`; code that
takes ownership of one invocation names `own callback: once fn(...)`. This RFC
does not introduce source-level capability/effect rows.

The compiler may internally track a closure environment's concrete type,
reference storage, ownership state, must-consume state, and effect summary. That
metadata proves the callable contract and selects a safe runtime layout; it is
not authority granted to the receiver and is not reflected as a list of root
capability names in the public function type.

If a future adapter genuinely needs to preserve a latent-effect variable across
input and output, a later RFC may add generic effect rows. This RFC reserves no
syntax for them. They are not required for ordinary delegation, purity, or
single-use invocation.

### Footprints and auditing

Capability tooling keeps two questions separate:

1. **Root demand:** which capability values must a public/root entrypoint
   receive?
2. **Delegated behavior:** which callback interfaces may the code invoke?

An ordinary callback parameter adds no guessed root capability to `witchy caps`.
A pure callback is visibly constrained by its type. A once callback is visibly
single-use. Tooling may report callable qualifiers in API documentation, but it
must not expand an opaque closure into the creator's captured roots and claim
the receiver possesses them.

The root program that creates a capability-bearing closure still requires the
captured capability through its actual lexical/data flow. Runtime launch grants
continue to originate at checked root parameters. This RFC changes neither
host grants nor Wasm import linking.

### Runtime representation and parity

Ordinary and pure reusable callables keep the existing immutable GC closure
wrapper and typed environment from RFC-0005. `pure` is erased after checking;
it needs no runtime bit.

`once` is a source-level affine contract. Both backends must reject use after
consumption through the shared checker, and compiled lowering may reuse the same
closure wrapper because no well-typed program observes a second call. No runtime
reference count, dynamic "already called" flag, destructor, or finalizer is
introduced.

Capability, ownership, and callable security are stable language semantics, so
RFC-0135 requires interpreter and compiled-Wasm agreement before this RFC can be
implemented. Checker rejection diagnostics are shared frontend evidence;
accepted invocation and higher-order transport require independent expected
results plus both backends.

### Diagnostics and reflection

Diagnostics should name the violated contract:

```text
closure declared `pure` invokes effectful callable `log`
use of once-callable `complete` after it was consumed by invocation
once-callable `complete` would be copied; pass it to an `own` parameter or move it
closure environment carries must-consume `transaction`; this callable type would erase that obligation
```

Formatting and reflection preserve `pure` and `once`. Callable type equality,
`meta.type_info`, documentation generation, `Dynamic` descriptors, trait method
signatures, and generated adapters must not silently erase either qualifier.

## Security model

This RFC does not give callbacks new authority. It makes the existing delegation
model explicit and adds constraints a caller may demand.

- An ordinary callback is intentionally effectful and opaque.
- `pure` prevents a supposedly deterministic/plugin callback from hiding an
  authority effect in its environment or downstream calls.
- `once` prevents replay through accidental or malicious repeated invocation.
- `own` prevents the caller from retaining its binding after transfer.
- `must` remains the mechanism for exactly-once disposition protocols.

Narrow callback interfaces remain the primary attenuation mechanism. A
`fn(String) -> Nil` grants the receiver control over arbitrary messages and
repeated calls. If the caller intends only a fixed notification, it should pass
`fn() -> Nil`; if it intends one completion, it should pass `once fn(Result) ->
Nil`; if it intends a domain operation, it should prefer a sealed nominal
object-capability interface over a stringly callback.

Purity does not authenticate code, confine native `Exec`, validate URLs or
paths, or make a computation terminate. Those remain artifact-integrity,
capability, validated-data, and resource-budget concerns.

## Alternatives

### Put captured capability rows on every callback

Rejected as the default model. It inverts delegation by requiring the callee to
describe authority chosen by its caller, leaks implementation detail, and
overstates what the receiver possesses. A closure backed by `Console` may expose
only `fn() -> Nil`; the receiver does not thereby own arbitrary console access.

Rows could describe latent effects rather than possessed authority, but ordinary
object-capability safety does not require them. A future effect-polymorphic
adapter may motivate a separate RFC.

### Make every closure capture-free

Rejected. Captured closures are the mechanism that attenuates a broad capability
to a narrow callable interface. Removing them would weaken rather than strengthen
the object-capability model.

### Treat every top-level function as pure automatically

Rejected. A top-level function can receive a capability and exercise it, call an
effectful function, or accept an opaque callback. Lexical capture is only one
effect source. `pure` is checked from the complete callable contract and body.

### Use `own` as the single-use marker

Rejected. `own` consumes the caller's binding at one call boundary; the callee
may still copy or invoke an ordinary function value repeatedly. Invocation
cardinality is a property of the callable value and requires `once`.

### Make `once fn` exactly-once by default

Rejected. Single-use authority often may be abandoned safely, and Rust-style
`FnOnce` prior art means "callable at most once." Exactly-once protocols already
compose from an affine `once fn` inside a nominal `must` wrapper, which can also
define explicit cancellation semantics.

### Documentation-only correction

Insufficient. Correcting the ordinary delegation rule fixes the conceptual
model, but APIs still need an enforceable way to require pure computation and
single-use invocation.

## Drawbacks

- Two new callable qualifiers enlarge parser, formatter, AST, type identity,
  reflection, monomorphization, trait, `Dynamic`, interpreter, and Wasm test
  matrices.
- `pure` requires a conservative effect classification for every reachable
  call. False negatives would violate the contract; false positives reject
  useful code and need good diagnostics.
- `once` adds an affine callable path to normal mode and must preserve ownership
  facts through aggregates, branches, generics, and indirect calls.
- Libraries must decide whether they need ordinary, pure, once, owned, or
  must-wrapped callbacks. The axes are principled but add vocabulary.
- Capability footprints intentionally cannot detect a dependency changing how
  it uses an already-delegated ordinary callback. That limitation must remain
  explicit in supply-chain documentation.

## Migration

Existing `fn` source keeps its meaning as an ordinary reusable, potentially
effectful callable. There is no bulk syntax migration merely to adopt this RFC.

Implementation requires one semantic documentation correction: claims that a
function without capability parameters is necessarily pure become claims that
code without direct or transitively delegated authority is powerless. Examples
that promise a plugin can "only compute" must take `pure fn`, require a bare
capture-free function through a narrower boundary, or weaken the promise.

Libraries adopt `pure` and `once` only where they make a real guarantee. The
compiler must reject any current path that transfers a must-consume capture into
an opaque callable while erasing the closure environment's disposition
obligation; compatibility does not outrank RFC-0114's safety contract.

## Acceptance criteria

1. The capability specification distinguishes possessed root authority from
   opaque callable delegation and removes the unconditional "no capability
   parameters means pure" claim.
2. Parser, formatter, AST, type equality, reflection, `Dynamic`, traits,
   generics, aliases, and documentation preserve `pure` and `once` without
   erasure.
3. `pure fn` accepts ordinary deterministic computation and immutable data
   captures; it rejects capability operations, opaque callback invocation,
   capability-bearing captures, and `var` parameter write-back with actionable
   diagnostics.
4. A pure callable widens to ordinary `fn`; ordinary effectful callables cannot
   narrow to pure through ascription, aliasing, branches, generics, or
   reflection.
5. `once fn` cannot be copied and is consumed on attempted invocation. Direct,
   indirect, generic, trait-backed, aggregate-held, branch-selected, and
   returned once callables reject a second use.
6. `own callback: fn(...)` remains reusable inside the callee, while `own
   callback: once fn(...)` transfers one affine invocation. Tests distinguish
   ownership transfer from call cardinality.
7. Ordinary and once closures reject affine or must-consume captures whose facts
   would be erased by the opaque function type. In particular, passing a lambda
   directly to `own callback: fn(...)` does not make a hidden must capture safe.
8. A nominal `must` wrapper around `once fn`, including a wrapper that holds an
   explicit must-consume resource beside the callback, proves exactly-once
   disposition or explicit cancellation on every CFG path without runtime
   finalizers.
9. `witchy caps` reports actual root/public capability demand and does not
   attribute a closure creator's hidden captures to a receiver of an ordinary
   callback.
10. Independent expected results, interpreter execution, compiled-Wasm
    execution, and forced-copy/optimization variants agree for the accepted
    higher-order matrix. Security-sensitive rejection diagnostics are shared
    frontend evidence and cannot be deferred under RFC-0135.
11. The installed documentation includes one pure-plugin example, one reusable
    delegated logger, one at-most-once completion, and one must-wrapped
    exactly-once protocol, each with an adjacent negative test.

## Prior art

- Rust's `Fn`, `FnMut`, and `FnOnce` separate callable behavior from ownership
  of a closure environment. Witchy's value semantics need no `FnMut` analogue
  because captured mutation cannot write back; `once fn` adopts only the
  consuming-call distinction.
- Object-capability systems treat an object reference as authority to invoke its
  interface, even when broader authority implements the object internally. This
  RFC applies that rule directly to closure values.
- Effect systems and row-polymorphic effects can describe latent effects through
  higher-order composition. This RFC deliberately takes the smaller explicit
  `pure` contract and leaves generic effect rows for a demonstrated future need.
- RFC-0114 supplies Witchy's existing must-consume disposition contract; this
  RFC composes with it rather than inventing destructor or finalizer semantics.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code, NOT here.
-->
