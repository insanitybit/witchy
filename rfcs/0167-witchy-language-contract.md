---
rfc: 0167
title: "Witchy syntax and semantics: one language contract"
status: accepted
created: 2026-09-16
superseded-by:
tracking: "Accepted language contract; not a compiler-completion claim. Implementation gates are in 0167-acceptance-ledger.md. Optimization expansion is deferred."
---

# RFC-0167: Witchy syntax and semantics

## Decision and authority

This is the single target language contract. It selects the surviving syntax,
including the ownership rules and normal/opt boundary. There are no alternative
spellings or unspecified choices to resolve by consulting retired proposals.
`accepted` fixes these source choices as the target for implementation. It does
not mean the compiler implements them; the acceptance ledger remains the sole
closeout record.

The [acceptance ledger](0167-acceptance-ledger.md) distinguishes inherited
behavior, intentional changes, and unimplemented coverage. Current executable
behavior remains a separate question for source, tests, and `spec/`. An old
RFC's `implemented` label cannot establish support on the current compiler route.

Retired documents and their replacement sections are listed in
[retired-rfcs.md](retired-rfcs.md). Git retains their exact historical bodies.
Retained implementation, security, and runtime RFCs are subordinate on source
syntax and semantics; their algorithms, evidence, and outstanding obligations
are not canceled. In particular, this RFC does not weaken capability admission
or SafeWIR verification.

Examples below specify the target contract. They are fragments, not claims that
today's compiler accepts or executes the entire feature matrix.

### Adoption review record

The adoption review and its 2026-09-18 memory-model revision closed the
following ambiguities before acceptance:

- immutable views use only `&T` and `&[T]`; explicit lifetimes, `View(T, 'a)`,
  and mutable references are absent from the language;
- every view owns a retained backing-root handle plus an authenticated
  projection, so its validity is not a lexical-lifetime promise;
- opt callable provenance survives normal callers and separate compilation;
- `pure` excludes caller-visible mutation and effectful callbacks;
- affine captures require `once`, while `must` obligations stay type-visible;
- weakening `unique T` into `T` consumes the unique handle;
- region escape depends on the viewed root's origin, not where a carrier was
  formed; and
- `frozen` never grants copyability or erases affine or `must` obligations.

No unresolved objection remains from that review. Rows B-Q still require
current production-route implementation and evidence; this status change does
not close any of them.

### Runtime authority and executable evidence

Checked canonical SafeWIR is the sole executable language contract. Production
execution lowers checked source to canonical SafeWIR, validates it, encodes it
to admitted Wasm, and runs that Wasm. The canonical SafeWIR interpreter executes
the same validated program as a test oracle; it is not a second source-language
implementation.

Semantic acceptance uses a three-part witness, in this order:

1. an independent expected value, diagnostic, effect trace, or structural
   invariant that is not computed by either execution engine;
2. the validated canonical SafeWIR program executed by the canonical
   interpreter; and
3. the same validated program encoded as unoptimized admitted Wasm and executed
   by the production runtime.

Agreement between two engines is corroboration, not the expected result. A
shared lowering or checker defect can make both engines agree incorrectly, so
the independent expectation is primary. Optimized Wasm is an optional
additional comparison and cannot replace the unoptimized witness.

The source tree-walking interpreter is frozen at this decision. It receives no
new language semantics, including immutable rooted views, and its output is not
RFC-0167 acceptance evidence. It remains temporarily available only where the
current compiler still depends on its bundled implementation:

- compile-time and build evaluation, until those programs use canonical
  lowering and execution;
- linker and tooling pipeline support, which moves to a runtime-neutral crate.

Production `run`, `sandbox`, `test`, and the playground execute admitted Wasm.
`witchy parity` uses the canonical SafeWIR interpreter as its oracle; it never
revives the source tree walker as a selectable backend.

Compatibility and security fixes may keep those frozen uses operational, but
must not turn the tree walker back into a production runtime or add a parallel
implementation of new syntax. The end state runs compile-time code through the
same canonical checked route and deletes the tree walker.

## 1. Core syntax

Witchy is statically typed, expression-oriented, and indentation-sensitive.
Blocks begin with `:`; indentation is four spaces. The final expression gives
the block's value. Comments are `//` or `/* ... */`.

```text
fn classify(n: Int) -> String:
    match n:
        0 -> "zero"
        _ -> if n > 0: "positive" else: "negative"
```

Scalars are `Int` (wrapping signed 64-bit arithmetic), `Float`, `Bool`,
`Duration`, `String`, and `Bytes`. `()` is both the unit type and value;
`Nil` is not a second builtin spelling. Tuples have arity zero or at least two;
`(x)` is grouping. `List(a)`, `Dict(k, v)`, `Set(a)`, `Option(a)`, and
`Result(a, e)` use ordinary type application.

`type Name:` declares a nominal record or tagged union. `type Name = T` is an
alias, not a fresh identity. `sealed type` restricts construction to its defining
module, including construction by reflection and generated code. Public reads
and matching do not confer constructor authority.

```text
type Point:
    x: Int
    y: Int

type Shape:
    Circle(Int)
    Empty

let p = Point(x: 1, y: 2)
let q = Point(x: 3, ..p)
let record = .{x: 1, y: 2}
```

Structural records and anonymous tagged unions remain. Record fields are static
identifiers. Structural width conversion projects an ordinary owned record to
the exact target shape; it does not confer nominal or sealed identity. `var`
write-back is invariant in shape. Views never perform implicit width
projection: a view names the actual root, projection, and layout. Anonymous union
width conversion only widens a tag subset, preserves exact tag/payload identity,
and rejects incompatible variants or implicit narrowing. An anonymous union
type is written `.[Empty | Item(Int)]`. Shared record fields are invariant;
branches do not infer a common row. Structural type spread has one base-first
spread; duplicate fields require exactly equal normalized types. Value spread
updates an exact shape. Structural
shapes cannot launder capabilities or acquire arbitrary user-defined trait
implementations.

`let` declares an immutable binding; `var` declares a reassignable binding.
Local binding mutability is independent of payload qualifiers. Assignment
targets `var` bindings and their writable fields or indices.
Compound assignment evaluates its place once. Index/field write-back preserves
other fields and elements. Overlapping mutable arguments to one call are
rejected unless disjointness is proved; argument order is not alias permission.

Boolean operators are `&&`, `||`, `!`; there is no truthiness. Arithmetic,
comparison, ranges (`..`, `..=`), field/index access, calls, interpolation,
and explicit conversions retain their checked meanings. No mode changes integer
overflow, evaluation order, bounds failure, or numeric results.

### Layout and discard

Same-indent statement boundaries must be represented by the layout pass, not
guessed from the preceding expression's type. A leading dual-use glyph such as
`*`, `&`, or `-` on a new statement line starts that statement. Bracketed
expressions and explicit syntactic continuation remain expressions; a leading
`.`/`?`/`as` or infix-only token may continue the preceding expression. Match-arm
layout must distinguish a new `.Tag` pattern from a method continuation.

`let _ = expression` is the sole explicit discard form. The proposed `e;`
replacement is dropped. Unit-valued expressions can stand alone. A non-unit
result must be used or explicitly discarded, including a mutator's status or
displaced value. Discard never satisfies a `must` obligation. Optimizing away
construction of an unused result is optional and cannot omit effects or traps.

## 2. Functions, control flow, and abstraction

`fn`, closures, methods, `pub`, `import`, and `from ... import ...` survive.
Parameter types are declared; private return types may be inferred. Exported
contracts have explicit result types, except omission for unit. Explicit and
inferred signatures have identical ownership semantics.

Named generic functions and methods declare type parameters in brackets after
the name: `fn first[a](items: &[a]) -> Option(&a)`. Those names bind the
corresponding signature and body types. The brackets must be nonempty, names
must be distinct, and lifetime tokens are rejected. Type application remains
parenthesized (`Option(a)`); bracketed parameters are declaration syntax, not a
second type-application spelling.

Arguments evaluate left-to-right in written order, including keyword arguments;
labels only select parameter positions. Parameter defaults are closed constants
and cannot depend on caller state or earlier arguments. Defaults form a trailing
suffix and labels apply only to direct resolved calls. Defaults do not exist for
`var` parameters. Function values retain conventions and view-carrier contracts;
labels and defaults belong to declarations, not function-value identity.

Methods resolve from checked receiver types and imported type-owned APIs, not
arbitrary free functions or rendered type-name strings. Traits, `impl`, `where`,
argument-position `impl Trait`, and explicit `dyn Trait` remain. Existential
values carry authenticated witnesses; generic erasure cannot drop ownership,
capability, view-root, purity, or invocation-cardinality contracts. Existential
methods exclude method generics and nonreceiver `Self`; view results retain the
same authenticated root-plus-projection contract through witness dispatch.
existential upcasts follow declared supertraits, not unrelated shape matches.

`if`, exhaustive `match`, `while`, `while let`, `for`, comprehensions, `break`,
`continue`, and `return` retain one control-flow model. One pattern grammar
covers bindings and matching: names, `_`, literals, constructors, tuples,
record/list shapes, integer ranges, and or-patterns. Ordinary binding and loop
positions require irrefutable patterns; `if let`/`while let` accept refutable
ones. Floating-point literal patterns are rejected.

Recoverable failure is a value: `Option`, `Result`, `?`, contextual `? "message"`,
and lazy, right-associative `??`. Fallback unwraps `Option`/`Result` and discards
the unselected Result error. An early `?` return is a structured exit for ownership and
write-back checking. Unexpected failure traps; there are no catchable
exceptions or implicit finalizers. Return-type inference must not change views,
consumption, trap behavior, or ABI eligibility.

Proper tail calls have no remaining caller work, including write-back, view-carrier
cleanup, conversion, or error inspection. Qualifying calls must use bounded
control stack even when optional optimization passes are disabled. This is a
language obligation, not something implied by `unique` syntax. There is no
`fip` keyword or implicit constant-allocation promise.

## 3. Owned values and calls

Ordinary values have snapshot semantics. Physical sharing and copy-on-write
are permitted only when later writes cannot affect an earlier value. Affine
values cannot be duplicated just because ordinary data can be.

| Parameter | Caller/callee contract |
|---|---|
| `x: T` | Callee receives an immutable owned value; copyable caller values remain usable. Affine inputs transfer instead of copying. |
| `let x: T` | Call-scoped read access; the borrowed value cannot escape as owned `T`. An explicit lawful materialization can produce an independent value. |
| `var x: T` | Caller supplies a mutable place; the call exclusively takes its current value and writes the final value back on structured completion. |
| `own x: T` | Caller transfers its binding's value; the source becomes unusable unless reinitialized, in both source modes. |

`move x` explicitly transfers a value at an expression site. `own` is a callee
contract; `move` is a caller operation, not another type qualifier. `own` does
not assert unique backing storage: earlier ordinary snapshots can still exist.
For copyable `T`, an ordinary assignment can create a snapshot before transfer.

```text
fn consume(own text: String):
    let _ = text.length()

let original = "hello"
let saved = original
consume(original)
// original is consumed; saved still contains "hello"
```

`var` commits on ordinary return and error-value return, including `?`, but not
after an aborting trap. This is a call protocol, not a database transaction:
completed host effects or earlier calls are not rolled back. Nested write-backs
reconstruct the current root and preserve unrelated earlier argument effects;
they must not restore a stale root snapshot.

## 4. Unique and frozen

Qualifiers occur on types after the colon, not between a convention and a name.

```text
fn update(var xs: unique List(Int)):
    xs[0] = 7

fn build() -> unique List(Int):
    [1, 2, 3]

fn publish(own text: frozen String) -> frozen String:
    text
```

### Unique owned storage

`unique T` means the owned root storage has one owning handle. It is affine:
an owned transfer moves the guarantee. Coercion into ordinary `T` consumes the
unique handle and gives up its guarantee; ordinary copies may be made afterward.
It never leaves a usable unique handle beside a new owning alias. Forming an
immutable view preserves the viewed snapshot and temporarily prevents in-place
mutation of that backing storage; a later `var` mutation detaches if the view is
still live. Neither a raw refcount observation nor an untracked alias establishes
uniqueness.

Uniqueness is a root property, not a recursive promise about every child.
`unique List(String)` guarantees the list's owning handle, not each string's buffer.
Nested in-place mutation needs its own proof or a nested qualifier such as
`unique List(unique String)`. Scalars trivially satisfy uniqueness. View fields
in an owned shell retain their separate roots and projections.

Normal mode may establish a requested unique value by making an independent
copy of copyable storage. Opt mode must prove the required uniqueness or require
an explicit materialization; it cannot insert a hidden alias-repair copy.
Every return declared `unique` must establish the guarantee on every path.

Uniqueness removes copies needed solely to repair aliases. It does not promise
spare capacity, no allocation, no relocation, or deep uniqueness. Growing a full
list can allocate and relocate. Layout, allocation budgets, destination passing,
and optimizer quality remain separate concerns.

`local unique T` is removed. Nonescape is expressed by `let`/`var` call contracts
and compiler analysis; owned uniqueness is `unique T`. A unique `var` parameter may
return its final value by write-back, never by duplicating it into an additional
owned result.

### Frozen payloads

`frozen T` is deep payload immutability. Fields and reachable owned payloads
cannot be mutated through any path. Freezing establishes an immutable snapshot;
it cannot freeze another live alias by side effect. Normal code may detach
copyable storage; opt code requires proof or explicit copying.

Moving or replacing the whole frozen value is legal. Thus `own x: frozen T`
and `var x: frozen T` are legal, but the latter may only replace `x`, not mutate
`x.field` or `x[index]`. `let` is local binding immutability and is not deep
freezing. Copyable frozen containers are safe to share; freezing never grants
copyability to affine or must-bearing contents. Frozen capabilities do not exist
and freezing is not an effect-purity annotation for callables. Frozen aggregates
may contain immutable views. Freezing the shell does not change the viewed
root's type; the view already preserves its own snapshot.

```text
fn replace(var text: frozen String):
    text = "replacement"
    // text[0] = ... is forbidden
```

`unique frozen T` and `frozen unique T` are rejected: this contract deliberately
does not add an unaliased-immutable ownership class. Use unique construction,
then transfer into a frozen snapshot. Freezing affine/must payloads cannot
silently make them copyable or erase their disposition obligations.

## 5. Immutable rooted views

Witchy has two immutable view types and one view expression in both modes:

```text
&T              // immutable view of one value
&[T]            // immutable contiguous view of zero or more T values
&place          // capture the current value or projection as a view
```

A view is a value-semantic snapshot handle, not a lexical loan. Its runtime
meaning is an authenticated backing root plus an authenticated projection. The
carrier retains the root for as long as any copy of the view remains live.
Consequently a view may be returned, stored in an aggregate, transported through
a generic, and captured by a callable without naming a lifetime. A view never
grants mutation authority and never exposes a raw address.

```text
fn first[a](items: &[a]) -> Option(&a):
    if items.length() == 0: None else: Some(&items[0])

let middle = &items[2..5]
```

The projection is bounds-checked when formed and remains tied to the retained
root. Field and element views retain the actual root and exact projection rather
than copying a displayed field name or index. Slicing a slice composes checked
projections. Empty slices are valid. An implementation may use another proven
equivalent representation, but authentication, root retention, projection
identity, and value semantics are mandatory at every source and ABI boundary.

### Snapshot and mutation interaction

`let` controls binding immutability. `var` controls local rebinding and parameter
write-back. Neither is a view qualifier. Taking `&` from a `var` captures the
then-current snapshot. A later mutation through the `var` preserves all existing
views by copy-on-write when storage is shared; it does not invalidate, retarget,
or mutate those views.

```text
var items = [1, 2, 3]
let before = &items[0..2]
items[0] = 9              // detach if required
// before still observes [1, 2]

fn increment(var value: Int):
    value = value + 1
```

`var` is the sole mutation and write-back syntax. It never coerces to a mutable
reference because mutable references do not exist. There is no `&mut`, no
`&'a T`, no `&'a mut T`, and no write-through dereference operation. A function
that changes its caller-visible argument declares `var`; a function that only
observes a retained projection accepts `&T` or `&[T]`.

Parameter conventions apply to the view value:

| View parameter | Meaning |
|---|---|
| `view: &T` or `view: &[T]` | Pass a copy of the immutable root-plus-projection carrier. |
| `let view: &T` | Borrow the local carrier for the call without changing the observable view semantics. |
| `own view: &T` | Consume that carrier binding; the referent remains immutable and independently rooted by any other view copies. |
| `var view: &T` | Replace the caller's view carrier by write-back; it does not mutate the viewed value. |

`unique &T` and `frozen &T` are rejected outer qualifiers: view access is always
immutable and copying a view shares only its retained snapshot. `&frozen T` and
`&unique T` are legal views of those payload types, but do not manufacture a
second unique owner or weaken frozen semantics.

`.owned()` explicitly materializes an independent owned value when the referent
type supports copying. The result has no view carrier. It cannot copy `must`
resources or authority lacking a lawful copy operation, and it cannot erase a
nested view by pretending an aggregate is deeply owned; such conversions require
an explicit checked mapping.

### Removed borrow spellings

`View(T, 'a)`, lifetime parameters, `&'a T`, `&'a mut T`, `&mut place`,
write-through `*reference`, `let('a) T`, lifetime-bearing `let('a) x: T`, and
`var('a) x: T` are removed, not aliases. A migration command may parse historical
input in a separate migration-only grammar; ordinary `check`, build, formatter,
and generated-code admission reject it after cutover. Migration must distinguish
immutable views from mutation/write-back and rewrite the latter to `var`; no
historical mutable-reference spelling may silently become a view.

## 6. Normal and opt interoperation

`mode opt` is a module source contract, not an optimizer switch. Immutable view
syntax has the same meaning in both modes. Opt mode additionally rejects missing
proofs for requested ownership and no-hidden-copy contracts. It does not promise
every operation is allocation-free. Turning
optional optimization passes on or off cannot change accepted source semantics.

Normal code imports opt modules and calls their exports using the declared value
conventions. Immutable views may cross a source-linked boundary only through the
root-plus-projection carrier in the signature; they are never detached into an
owned value or erased from a nested type. A separately compiled ABI may reject
view-bearing exports until its authenticated metadata and carrier support them,
but it cannot silently change the signature.

```text
// fast.witchy
mode opt

pub fn set_first(var xs: unique List(Int)):
    xs[0] = 9

// app.witchy (normal)
import fast

fn demo() -> Int:
    var xs = [1, 2]
    let snapshot = xs
    fast.set_first(xs)
    snapshot[0] + xs[0]       // 10; snapshot is unchanged
```

The call protocol is:

1. Evaluate arguments once in written order and validate conventions.
2. On the normal caller side, establish callee preconditions. Reuse storage when
   proved safe; otherwise detach copyable storage. Never copy affine resources
   or authority-bearing resources to repair a contract. Existing immutable
   views already own their snapshot carrier and require no alias repair.
3. Enter the opt body with the same proven contract as an opt caller supplies.
   Its body has no hidden alias-repair fallback.
4. Return owned results and commit `var` write-backs on structured completion.
   Preserve snapshots, authority, consumption, and error semantics throughout.

An opt caller of the same export must supply the proof or explicit copy itself.
A failed proof is not a runtime trap or an automatic switch to normal semantics.
Generic instantiation, callbacks, trait witnesses, and function values preserve
these entry contracts. Copy preparation belongs to a normal call site, not a
second function identity that can leak through a function pointer.

Opt modules may depend on opt modules and authenticated standard-library
contracts. They do not import arbitrary normal user modules or invoke
unqualified normal callbacks whose hidden repair behavior would defeat their
contract. A normal higher-order caller can receive an opt function through a
convention-preserving interface; an opt higher-order caller
requires a checked opt-compatible callable contract. No new surface `opt fn`
spelling is introduced: provenance is retained in checked callable identity,
and erasing it cannot make a call admissible.

Specifically, a callable parameter declared in an opt module requires an
opt-compatible callable, including nested callable inputs/results. A normal
caller passing an arbitrary normal closure fails that precondition; copy
preparation cannot repair it. Exported signature metadata carries this rule so
separate compilation never relies on inspecting the callee's body.

The normal root does not disable an imported module's source checks. Conversely,
an opt dependency does not opt the normal root into stricter syntax. The runtime
carrier ABI must preserve these facts without relying on optional inlining,
specialization, ownership extraction, or bounds-check elimination.

## 7. Resources, effects, and suspension

`must type T` makes disposition an obligation. No live instance may be dropped,
overwritten, or discarded on a continuing/structured-exit path. Transfer moves
the obligation; borrowing leaves it with its owner. Both success and error-value
returns from a consuming operation must account for it. Unconditional program
abort is not an implicit cleanup protocol. `must` propagates through aggregates
and admitted suspension frames; callable capture restrictions are specified
below. No implicit destructor or finalizer is added.

`pure fn(A) -> B` forbids externally observable authority effects and caller
mutation, including `var` write-back. Immutable view reads are permitted. It
cannot invoke effectful callbacks. Local mutation and allocation are allowed.
Ordinary
callbacks may delegate effects through captured capabilities without exposing
the capabilities themselves. `once fn(A) -> B` permits at most one invocation,
which consumes the callable. `own callback` transfers a callable but does not
alone restrict invocation count. A once callable can be abandoned only if it
does not carry a `must` obligation. Combining `pure` and `once` preserves both
contracts. These qualifiers apply to synchronous callables, not new async or
generator qualifier syntax.

Capturing an affine value requires a `once` callable; a reusable callable cannot
consume the same capture repeatedly. A `must` value cannot be hidden in an
opaque function type, even `once fn`: use an explicit must-bearing nominal
wrapper with checked consuming operations. No cast or generic erasure can turn
that wrapper into an ordinary discardable callable. Frame propagation below
requires an obligation-visible frame contract and checked cancellation paths.

Capabilities are unforgeable authority supplied through typed host entrypoints
and delegated values. Narrowing never widens rights; sealed constructors,
reflection, Dynamic, and metaprogramming cannot forge authority. No capability
parameters does not imply purity when effectful callbacks are accepted.
Detailed rights, grants, confinement, and host authentication retain their
independent security contracts.

`async fn` returns a lazy `Task(T)` recipe; `.await` drives it. A recipe may be
reused only with copyable captures. Capturing affine or must resources into a
replayable recipe is rejected, not implicitly cloned. Cooperative tasks and
typed `Sender(T)`/`Receiver(T)` channels have deterministic scheduling within one
VM, owned messages, bounded-channel backpressure, and explicit cancellation/join
semantics. Quiescence, not last-sender destruction, governs channel closure.
True parallel work uses explicitly isolated worker VMs, serialization, and
explicit capability grants. Cooperative `par_map` is not CPU parallelism.

The proposed RFC-0161 changes to must-start Task values, scoped Child types,
explicit channel-owner close, and selectable-operation syntax are not selected
by this consolidation. They remain a separate deferred surface decision; their
implementation/evidence ledger is preserved. Adopting them must amend this
section first. Typed-runtime representation work that preserves the selected
semantics remains independent. This makes the quiescence choice explicit rather
than treating both closure models as simultaneously normative.

`gen fn ... -> Iter(T)` and `yield` remain. A generator resumes an owned frame;
it never replays earlier body effects to find the next yield. `Iter(T)` yields
owned values and is not a lending iterator. `collect` uses `FromIterator` and
the expected result type. Trait APIs return `Task(T)` or `Iter(T)` through
ordinary methods, not dedicated async/gen trait syntax.

A bare generator `return` ends iteration; returning a value is rejected.

Rooted immutable views may be captured by synchronous callables and stored in
ordinary aggregates. Suspension and channel carriers require an authenticated
retained-root representation before admitting a view; isolated workers and
`Dynamic` require an explicit owned or serialized value instead. `var`
parameters cannot span suspension. Unsupported carriers reject rather than
erasing a view.
Cancellation must not silently abandon `must` captures: require a checked
cleanup/transfer path, or reject creation of the frame.

## 8. Regions, layout, reflection, and libraries

`region:` and `region -> T:` survive as explicit lexical allocation scopes.
The final value escapes by owned promotion/copy-out; parent-owned parts may pass
through. A view of a region-born root never escapes because retaining it cannot
extend storage past rewind. A view created inside a region may refer to a
parent-owned root and leave with its retained root. Region-born pointers cannot be
assigned into outer ordinary bindings; scalar assignment is allowed. Suspension
or yield cannot carry region storage past rewind. Capability effects are not
rolled back. Opt mode cannot conceal promotion copies: prove a copy-free result
or materialize it explicitly. Reclamation strategy and allocation savings remain
measured implementation properties, not a second value semantics.

`type P packed:` remains an opt-only representation declaration for admitted fixed
layouts. Its fields, alignment, size, and cross-call carrier must be checked
end to end; no unchecked reinterpretation or integer/reference bit-cast is
authorized. Unsupported shapes get a diagnostic. Layout expansion, SIMD, FIP,
destination passing, and further optimizer passes are deferred and are not
prerequisites for deciding the source contract.

`Reflect`/`Mirror` provide explicit structural inspection, and `Deserialize`
provides checked construction. `comptime:`/`comptime fn`, typed `quote`, derives,
and tagged literals remain. Compile-time evaluation is deterministic, bounded,
and capability-free. Typed syntax carries hygiene and diagnostic provenance;
generated bodies re-enter ordinary source checking before lowering. Compile-time
generation is additive: it cannot rewrite existing declarations. Raw
`emit(String)` and source-string syntax builders are retired from the target
surface; use typed syntax. Compile-time syntax values cannot escape at runtime.

`Dynamic` is an explicit authenticated descriptor plus owned payload, not an
implicit fallback for failed static checking. Checked decode, public inspection,
registered `@dynamic` methods, and authenticated trait queries return structured
errors on mismatch. A registered method may expose a view result through static
reflection, but dynamic invocation refuses it with `DynamicError.BorrowedResult`
before decoding or calling the method; a Dynamic payload never carries a live
view carrier. It cannot hide views,
manufacture capabilities, expose sealed constructors, or rebind nominal identity
by display name.
Reflection is not arbitrary dynamic code loading.

Standard-library protocols (`Show`, equality/order/hash, `Reflect`, conversions,
`Deserialize`, and `FromIterator`) have one checked meaning for direct, method,
generic, and indirect calls. Pure data APIs do not obtain ambient capabilities;
equality recursively uses `PartialEq`; dictionary keys and set members require
`Eq`. Functions and capabilities are not comparable; Float does not satisfy
the `Eq` key contract. Display uses interpolation or `Show`, not a private
untyped rendering bypass. Collection lengths use `.length()` consistently;
effect APIs take authority explicitly. Absence is `Option`, recoverable failure
is a structured `Result`, and broken preconditions trap only where documented.
Target-specific declarations and `@browser`/`@server`/`@static` availability are
checked through calls and function values, not selected by accidental linkage.
In-language `test` declarations and runnable documentation remain part of the
tooling contract.

## 9. Deliberate changes and tradeoffs

This consolidation removes lifetimes, mutable references, `local unique`, raw
string code emission, and the unimplemented semicolon-discard proposal. It
chooses `()` over builtin `Nil` while retaining private return inference
(omission does not unconditionally force unit). It makes `own` consume in both modes, permits
whole-value transfer/replacement of frozen values, rejects qualifier wrappers
on immutable views, and makes `var` the sole mutation/write-back syntax.

These choices require one-cut migration and reject some programs accepted by
the existing frontend. Keeping all historical forms would retain the conflicting
contracts that prompted this RFC. Replacing `var` with mutable references would
create two mutation protocols and expose lifetime reasoning without changing
value semantics. Keeping hidden view-to-owned adapters would give one function
two observable type contracts.

Rooted views deliberately pay a safe retain when needed. Implementations may
elide retains, copy-on-write detaches, and carrier materialization only after
proving that all observable snapshots and root lifetimes remain unchanged.
Normal value programs retain copy-correct fallback, but cannot use it to
duplicate authority/resources.

The acceptance ledger, not the length of this RFC or a green subset of tests,
determines when the selected language is implemented.

## 10. Optimization readiness is part of the contract

> 2026-09-16: Optimization implementation is deferred; preserving its semantic
> prerequisites is required now. This section adds no measured speed claim.

Both normal and opt source may benefit from optimization. `mode opt` selects
source-level obligations, not exclusive access to a faster backend. The normal
root must not semantically prevent an imported opt function from being
optimized. Optional pass selection cannot alter which source contracts hold.

### Preserve freedom of representation

An owned value copy promises an independent snapshot, not a mandatory allocation,
buffer copy, retain/release pair, or box. `.owned()` establishes independence;
its physical copy may be elided or delayed when all later observations remain
equivalent. A view denotes a retained logical root and projection, not a mandated
heap object or raw-pointer ABI. Source cannot observe pointer identity,
allocation addresses, reference counts, or compiler-private ownership tokens
through ordinary value operations. Diagnostic instrumentation may report physical
costs without making storage identity part of value semantics.
Explicit checked layout contracts remain binding where they apply.

Consequently, ordinary records can be scalar-replaced, nonescaping values and
closures can use registers/stack/regions, and owned results can be constructed
directly into their final destination. `var` write-back need not materialize a
temporary aggregate or transaction buffer when direct mutation preserves every
observable snapshot, effect, and exit. There is no rollback of host effects.

One source callable identity does not require one physical entry or carrier.
Internal specialized entries, direct calls, and verified boundary adapters are
permitted. A normal-to-opt call with established preconditions can enter directly,
without mandatory cloning or boxing; a copyable aliased input can use the normal
repair path. Function values, traits, and separate compilation must preserve
the same contract, even when a particular opaque call cannot be specialized.
No adapter may manufacture authority, duplicate affine values, or erase a view.

### Facts the compiler must retain

Checked source, exported summaries, and intermediate representations must retain
enough information to express the following opportunities without new source
syntax. A pass can decline an opportunity; lowering must not erase its semantic
preconditions merely because that pass has not been built yet.

| Source information | Future transformation enabled | Required limitation |
|---|---|---|
| `own x: T`, last use, and `unique T` | Move/copy and reference-count elimination; in-place update/extraction; destination passing | Consumption alone does not prove uniqueness; nested storage needs independent proofs; growth may allocate. |
| `&T`/`&[T]`, retained roots, projections, and proven nonescape | Retain elision, borrowed call transport, stack allocation, scalar replacement, closure-allocation elimination | Preserve snapshot semantics and exact projection bounds; mutation must detach while any observing view remains live. |
| `frozen T` and checked effects | Shared immutable storage, redundant-load elimination, common-expression reuse, loop-invariant motion | Freezing does not prove purity, totality, copyability, or immutability of unrelated referents. |
| `var` plus checked uniqueness and disjoint places | In-place update, alias-sensitive load/store optimization, and vectorization | A live view forbids an in-place write that would change its snapshot; exclusivity covers only the proven place and interval. |
| Concrete generic types, layouts, and authenticated callable/witness identity | Monomorphization, unboxing, inlining, devirtualization, packed access | Preserve public logical types, specialized layout contracts, ownership envelopes, and capability identity. |
| Bounds/range facts tied to the current owner generation | Bounds-check elimination, loop simplification, vectorized kernels | Mutations and unknown calls invalidate affected facts; preserve wrapping integer and strict floating-point semantics. |
| Explicit discard and observable-effect information | Eliminate unused result construction and dead stores; fuse iterator/collection work | Retain effects, traps, termination behavior, and `must` obligations; `pure` alone does not permit speculation. |
| Root provenance and typed owned resumable state | Region/destination inference, frame-slot reuse, direct resumable lowering | A region-born root cannot escape rewind; preserve suspension points, deterministic scheduling, cancellation, and one-time effects; no replay. |

The compiler may infer uniqueness and nonescape in ordinary source. Removing
`local unique` removes a spelling, not the nonescape fact. Likewise, rejecting
`unique frozen T` as surface syntax does not prohibit the compiler from knowing
that immutable storage currently has one owner. Freeze can transfer uniquely
owned storage without copying when its complete immutable snapshot is proven.
Such facts must be invalidated or recomputed when aliasing, writes, escapes,
calls, or control-flow joins invalidate their assumptions.

### Acceptance before adding optimization passes

For each table row, provide a positive source witness, a near-miss that forbids
the transformation, and the exact checked-summary/IR fact carrying the proof
across its relevant boundaries. Include normal/opt imports, indirect/generic
calls, nested aliases, and early exits. Unsupported paths remain explicit gaps,
not cases repaired by inventing optimizer facts after source information is lost.

Unoptimized execution must independently satisfy the language and mandatory
resource contracts. Later passes must revalidate transformed safety evidence and
preserve values, source-ordered effects/traps, termination, authority, and
scheduling. Proper-tail-call stack bounds and explicit no-alias-repair promises
are correctness obligations even without an optional optimizer.

Performance promotion is separate: use matched workloads with independent
expected results, before/after timing and allocation/copy counters, scaling
cases, and inspection of the generated code when relevant. Count normal-side
preparation as part of end-to-end cost; do not report only a fast opt body while
excluding its boundary copies. Runtime timing, allocation, cache, I/O, SIMD, and
concurrency measurements are inapplicable to this documentation amendment; no
production code or performance result changes here.
