---
rfc: 0167
title: "Witchy syntax and semantics: one language contract"
status: proposed
created: 2026-09-16
superseded-by:
tracking: "Complete target-design draft; not a compiler-completion claim. Implementation and adoption gates are in 0167-acceptance-ledger.md. Optimization expansion is deferred."
---

# RFC-0167: Witchy syntax and semantics

## Decision and authority

This is the single target language contract. It selects the surviving syntax,
including the ownership rules and normal/opt boundary. There are no alternative
spellings or unspecified choices to resolve by consulting retired proposals.
`proposed` means the design still needs review and adoption, not that the choices
below are optional. This documentation change does not implement them.

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
write-back is invariant in shape. References never perform implicit width
projection: a reference names the actual place and layout. Anonymous union
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
targets variables, fields, indices, and (in opt mode) dereferenced places.
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

Arguments evaluate left-to-right in written order, including keyword arguments;
labels only select parameter positions. Parameter defaults are closed constants
and cannot depend on caller state or earlier arguments. Defaults form a trailing
suffix and labels apply only to direct resolved calls. Defaults do not exist for
`var` parameters. Function values retain conventions and type/loan relations;
labels and defaults belong to declarations, not function-value identity.

Methods resolve from checked receiver types and imported type-owned APIs, not
arbitrary free functions or rendered type-name strings. Traits, `impl`, `where`,
argument-position `impl Trait`, and explicit `dyn Trait` remain. Existential
values carry authenticated witnesses; generic erasure cannot drop ownership,
capability, lifetime, purity, or invocation-cardinality contracts. Existential
methods exclude method generics, nonreceiver `Self`, and borrowed-self results;
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
exceptions or implicit finalizers. Return-type inference must not change loans,
consumption, trap behavior, or ABI eligibility.

Proper tail calls have no remaining caller work, including write-back, loan
cleanup, conversion, or error inspection. Their control-stack bound is a
separate backend obligation, not something implied by `unique` syntax. There
is no `fip` keyword or implicit constant-allocation promise.

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
It never leaves a usable unique handle beside a new owning alias. Scoped shared
loans are allowed, but suspend conflicting owner
operations until their last use. Exclusive loans permit one active access path.
Neither a raw refcount observation nor an untracked alias establishes uniqueness.

Uniqueness is a root property, not a recursive promise about every child.
`unique List(String)` guarantees the list's storage, not each string's buffer.
Nested in-place mutation needs its own proof or a nested qualifier such as
`unique List(unique String)`. Scalars trivially satisfy uniqueness. Borrowed
fields in an owned shell retain their separate referent loans.

Normal mode may establish a requested unique value by making an independent
copy of copyable storage. Opt mode must prove the required uniqueness or require
an explicit materialization; it cannot insert a hidden alias-repair copy.
Every return declared `unique` must establish the guarantee on every path.

Uniqueness removes copies needed solely to repair aliases. It does not promise
spare capacity, no allocation, no relocation, or deep uniqueness. Growing a full
list can allocate and relocate. Layout, allocation budgets, destination passing,
and optimizer quality remain separate concerns.

`local unique T` is removed. Nonescape is expressed by `let`/`var` call contracts
or explicit loans; owned uniqueness is `unique T`. A unique `var` parameter may
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
and freezing is not an effect-purity annotation for callables. Frozen aggregate
types reject exclusive-reference fields. Shared-reference fields retain their
ordinary external owner loans: freezing the shell does not freeze those owners.

```text
fn replace(var text: frozen String):
    text = "replacement"
    // text[0] = ... is forbidden
```

`unique frozen T` and `frozen unique T` are rejected: this contract deliberately
does not add an unaliased-immutable ownership class. Use unique construction,
then transfer into a frozen snapshot. Freezing affine/must payloads cannot
silently make them copyable or erase their disposition obligations.

## 5. References and lifetimes

Only `mode opt` source names references or lifetime parameters. There are two
reference types and two reference expressions:

```text
&'a T          // shared reference
&'a mut T      // exclusive reference
&place         // open shared loan
&mut place     // open exclusive loan
*reference     // access the referent place
```

A reference is an access handle to a place, never an owned `T`. Borrow creation
requires a stable owner and the necessary alias/disjointness proof. A shared
loan prevents overlapping mutation or owner invalidation. An exclusive loan
excludes other overlapping access. Shared references are copyable; exclusive
references are affine. Reborrowing shortens access and suspends conflicting
parent access. An exclusive-to-shared reborrow is permitted, never the reverse.
Owner use resumes when all relevant loans end, not when an arbitrary local
binding goes out of scope. Escape, branches, loops, and early exits preserve the
same facts conservatively.

Lifetimes name relations, not allocator choices or a duration literal. Named
lifetimes used in function input types (including nested nominal arguments)
bind the relation available to the result. A result cannot invent a fresh owner
or extend an input's life. First-class reference annotations always name their
lifetime; there is no second elided type spelling in this contract. Borrow
expressions infer their relation from context.

```text
mode opt

fn edit(value: &'a mut String) -> &'a mut String:
    value

type Parser('a):
    input: &'a String
    offset: Int

// Parser('a) owns its shell. &'b Parser('a) borrows that shell.
```

Nominal lifetime parameters survive, including combined ordinary/lifetime
parameters. `String('a)` is rejected because `String` declares no lifetime
parameter. Borrowed aggregates keep each field's owner and access relation
through construction, projection, replacement, calls, and returns. An owned
shell containing a shared reference may move or update unrelated owned fields;
that does not mutate the referent. An exclusive-reference field makes the shell
affine. Dynamic index loans conservatively cover all possible selected roots.

### Handle operations versus referent operations

Parameter conventions apply to the handle, not automatically to its referent:

| Reference parameter | Meaning |
|---|---|
| `r: &'a T` | Copy the shared handle, preserving the owner's loan. |
| `r: &'a mut T` | Reborrow exclusive access for the call/result relation; parent access resumes after that reborrow ends. |
| `let r: &'a T` | Borrow the shared handle for the call; copying its shared referent handle is permitted only within `'a`. |
| `let r: &'a mut T` | Read-only borrowing of the exclusive handle; no referent write or exclusive-handle escape. |
| `own r: &'a T` or `own r: &'a mut T` | Move the handle and consume the source binding; never acquire ownership of `T`. |
| `var r: &'a T` or `var r: &'a mut T` | Replace the caller's handle, subject to its full lifetime/access relation; do not reinterpret `var` as a write through `r`. |

Writing through an exclusive reference uses `*r = value`; replacing a handle
uses `r = another_reference` in a `var` handle parameter. A replacement must
outlive the caller's remaining uses. Returning an additional exclusive alias
while also writing that handle back is forbidden. Plain exclusive parameters
are reborrows, not implicit consuming `own` parameters.

`unique &'a T`, `unique &'a mut T`, and `frozen &'a T` are rejected outer
reference qualifiers. Reference access already determines handle aliasing.
`&'a frozen T` is legal and promises an immutable referent; `&'a mut frozen T`
is rejected. `&'a unique T` and `&'a mut unique T` refer to uniquely owned
storage and retain its owner contract without manufacturing a second owner.

`.owned()` explicitly materializes an independent owned snapshot when the type
supports copying. Future writes must preserve it even if its representation
shares storage initially. It cannot copy exclusive handles, `must` resources,
or authority lacking a lawful copy operation, and cannot erase nested references
by pretending an owned shell is a deeply owned value. Materializing a borrowed
aggregate into a reference-free counterpart requires a checked conversion.

### Removed reference spellings

`View(T, 'a)`, `let('a) T`, lifetime-bearing `let('a) x: T`, and
`var('a) x: T` are removed, not aliases. A migration command may parse historical
input in a separate migration-only grammar; ordinary `check`, build, formatter,
and generated-code admission reject it after cutover. Migration must rewrite
declarations, calls, borrows, and dereferences together; textual type replacement
alone is insufficient.

## 6. Normal and opt interoperation

`mode opt` is a module source contract, not an optimizer switch. It admits
explicit references and rejects missing proofs for requested ownership/access
contracts. It does not promise every operation is allocation-free. Turning
optional optimization passes on or off cannot change accepted source semantics.

Normal code imports opt modules and calls their reference-free exports using
ordinary value conventions. References, lifetime-bearing types, and hidden
borrowed fields cannot cross that public value interface. An opt library needing
both interfaces writes a value wrapper and a separately named reference API;
there is no silently changed return type or implicit borrowed-result detachment.

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
   or active exclusive references to repair a contract.
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
reference-free, convention-preserving interface; an opt higher-order caller
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
abort is not an implicit cleanup protocol. `must` propagates through aggregates,
closures, and suspension frames; no implicit destructor or finalizer is added.

`pure fn(A) -> B` forbids externally observable authority effects and caller
mutation, including `var` write-back and writes through input references. It
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

This contract does not admit source references into suspended task/generator
frames, channel payloads, isolated workers, or Dynamic. `var` parameters cannot
span suspension. Materialize lawful owned payloads before these boundaries.
Reference-bearing synchronous closures may exist within their checked loans;
they cannot erase those loans through an apparently reference-free escape.
Cancellation must not silently abandon `must` captures: require a checked
cleanup/transfer path, or reject creation of the frame.

## 8. Regions, layout, reflection, and libraries

`region:` and `region -> T:` survive as explicit lexical allocation scopes.
The final value escapes by owned promotion/copy-out; parent-owned parts may pass
through. A reference whose owner is region-born never escapes. A handle created
inside a region may still refer to a parent-owned root and leave under its
ordinary loan relation. Region-born pointers cannot be
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
errors on mismatch. It cannot hide active references, manufacture capabilities,
expose sealed constructors, or rebind nominal identity by display name.
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

This consolidation removes duplicate lifetime spellings, `local unique`, raw
string code emission, and the unimplemented semicolon-discard proposal. It
chooses `()` over builtin `Nil` while retaining private return inference
(omission does not unconditionally force unit). It makes `own` consume in both modes, permits
whole-value transfer/replacement of frozen values, rejects qualifier wrappers
on reference handles, and fixes `var &mut` to mean handle write-back.

These choices require one-cut migration and reject some programs accepted by
the existing frontend. Keeping all historical forms would retain the conflicting
contracts that prompted this RFC. Removing value `var` in favor of references
would unnecessarily expose lifetime reasoning to normal code. Keeping hidden
borrowed-result adapters would give one function two observable type contracts.

Reference handles still have a deliberate affine reborrow rule for plain
exclusive parameters. Its exception to default owned-value transfer is stated
in the type contract and tested; it must not be confused with `own` consumption.
Conservative loan proofs can reject valid opt programs. Normal value programs
retain copy-correct fallback, but cannot use it to duplicate authority/resources.

The acceptance ledger, not the length of this RFC or a green subset of tests,
determines when the selected language is implemented.
