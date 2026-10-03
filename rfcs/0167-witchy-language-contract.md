---
rfc: 0167
title: "Witchy syntax and semantics: one language contract"
status: accepted
created: 2026-09-16
superseded-by:
tracking: "Normal-mode language contract is the current delivery target; opt mode and its dependent features are deferred. Milestones and evidence are tracked in 0167-acceptance-ledger.md."
---

# RFC-0167: Witchy syntax and semantics

## Decision and authority

This is the normal-mode target language contract. It selects the surviving
syntax, including shared ownership rules. RFC-0173 owns deferred opt-specific
rules. There are no alternative spellings or unspecified choices to resolve by
consulting retired proposals.
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

### Current delivery focus

Finish normal-mode semantics for snapshot values, `own`, `unique`, `var`
write-back, immutable rooted views, and `must` disposition. `must` is a core
typing rule: a value cannot be discarded, hidden in an opaque carrier, or left
live across a path without a checked disposition. Task cancellation needs a
separate lifecycle design where the current rules cannot prove disposition;
unsupported cases must refuse. Complete the remaining normal-mode ownership
and view carrier cases recorded in the acceptance ledger before claiming the
full contract implemented.

Capabilities and effects are also source-language semantics. This RFC owns the
meaning of typed capability parameters, effectful calls, delegation, and
purity. [RFC-0126](0126-capability-effects-contract.md) owns the independently
reviewable host boundary: authenticated roots, rights narrowing, footprints,
grants, and confinement. Neither document may silently weaken the other.

The bounded typed `comptime` forms needed by the normal-mode compiler remain
in scope. Broader generated syntax, region promotion, packed layout, broad
reflection, and runtime `Dynamic` are not current acceptance gates; existing
supported cases retain their safety rules and unsupported cases refuse. Internal
layout validation is still required by SafeWIR. `mode opt` and cross-mode
matrices remain deferred as specified below.

### Delivery decision, 2026-09-29: defer opt mode

The current delivery target is the normal-mode language, canonical compiler,
runtime, and tooling. `mode opt` is a deferred source contract, not a required
route to fast code. Both modes use the same optimization backend; normal-mode
code may use checked ownership facts, in-place updates, representation changes,
and other legal optimizations. A performance improvement cannot depend on a
file being rewritten into opt mode unless a separately measured workload shows
that the opt contract itself is necessary.

Defer new opt-only proof rules, no-hidden-copy diagnostics, opt callback and task
provenance, opt-specific generation and standard-library coverage, and the
normal-to-opt and opt-defined-type acceptance matrices. The selected rules in
this document remain the design for a later opt-mode milestone, not completion
criteria for the current normal-mode release. Existing implemented opt paths
may remain available, but are not evidence that the deferred contract is
complete; unsupported paths must refuse rather than silently weaken ownership
or authority. Do not remove checks that protect normal-mode value semantics,
resource disposition, capability boundaries, or admitted Wasm safety.

`unique`, `own`, `var`, immutable views, copy-on-write, regions, `frozen`, and
resource obligations remain in scope where normal-mode programs use them.
Their value and safety rules are independent of opt mode. The acceptance ledger
separates the current milestone from deferred opt and wider proof work.

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

### Composition clarification, 2026-09-25

The user-authorized clarification below selects the remaining composition rules
for call-scoped access, resource-bearing views, and lazy task preparation. It
preserves snapshot values, immutable views, `var` write-back, and transitive opt
contracts. The new accepted/rejected examples specify the target; they are not
claims of current compiler support or new acceptance evidence. Rows G, I, J, K,
and P must account for these cases before their full contracts are closed.

The usability target is straightforward normal-mode data programming and bounded
ownership friction for explicitly selected opt code. Normal mode manages
retention, copying, and write-back for copyable data automatically.
Opt mode requires proofs or explicit operations where the declared contract
forbids implicit ownership repair. Both modes preserve the same values, effects,
evaluation order, and resource obligations whenever both accept a program.

Normal mode does not silently undo `own`, duplicate affine resources, discard
`must` obligations, or resolve conflicting mutable arguments by choosing a
write-back order. Such errors should identify the source value and operation,
not require the programmer to understand SafeWIR carriers. Unsupported compiler
representations are implementation gaps, not additional language semantics.

### Adversarial review clarification, 2026-09-26

The user-authorized review clarification preserves the selected snapshot,
view, write-back, and replayable-task contracts. It makes the normal/opt type
boundary and resource-disposition requirements explicit below. These are target
requirements, not new implementation evidence or completed acceptance rows.

Single execution, required disposition, and eventual completion are separate
properties. Resource-owning operations need a checked lifecycle, but that does
not select a universal must-start Task or remove replayable recipes. Section 12
records the decisions and migration witnesses required before changing that
surface. The [acceptance ledger](0167-acceptance-ledger.md) separates current-contract work from deferred
language choices. Ordinary-mode usability, opt usability, and performance remain
claims to establish with representative programs and measurements.

### Runtime authority and executable evidence

Checked canonical SafeWIR is the sole executable language contract. Production
execution lowers checked source to canonical SafeWIR, validates it, encodes it
to admitted Wasm, and runs that Wasm. There is no reference interpreter in the
active build.

Acceptance uses checked source compiled through validated canonical SafeWIR to
unoptimized admitted Wasm and executed by the production runtime. Current
milestone tests assert the intended values, diagnostics, and effects for
representative normal-mode programs. Opt and cross-mode programs belong to the
deferred opt milestone. Ownership and authority changes also need focused
checks at the boundary that grants or rejects them. An independently implemented
execution oracle, a canonical SafeWIR interpreter, and a separately derived
expectation for every supported case are not acceptance gates. Optimized Wasm is
an optional additional comparison and cannot replace production execution.

The first closeout milestone is **compiler tooling ready**: the canonical
`check`, `fmt`, compile, and run workflows work for representative programs;
production compile-time code uses the canonical checked route; typed syntax
handles support the public generation forms needed by those programs,
`type_info(T)`, and the reflected Builder example. Unsupported generation forms
must give a clear error rather than enter through source strings or unchecked
syntax. The source tree walker is retired after its remaining compiler services
move to the canonical route or runtime-neutral code. The whole book, example,
fixture, and external-consumer corpus is not a closeout gate for this milestone.

The selected ownership, view, and resource rules remain the normal-mode language
contract. The normal/opt boundary and its dependent proof matrix are deferred.
Wider memory-safety and optimization-readiness matrices are verified after
compiler tooling is ready. This staging grants no permissive
fallback: an unproved ownership, disposition, or authority path must refuse
until implemented and checked. Passing the tooling milestone does not mark the
full RFC implementation complete.

The source tree-walking interpreter is frozen at this decision. It receives no
new language semantics, including immutable rooted views, and its output is not
RFC-0167 acceptance evidence. It remains temporarily available only where the
current compiler still depends on its bundled implementation:

- compile-time and build evaluation, until those programs use canonical
  lowering and execution;
- linker and tooling pipeline support, which moves to a runtime-neutral crate.

Production `run`, `sandbox`, `test`, and the playground execute admitted Wasm.
If a future independent executable oracle is justified, it belongs in a
separate opt-in crate outside the routine workspace build and merge gate; it
must not restore the source tree walker as a selectable backend.

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

### Call-scoped access and retained results

`let` on a parameter promises that access obtained through that parameter does
not survive the call. This includes a view derived from it, a copy of a borrowed
view carrier, and any aggregate, closure, task, or message containing that access.
Renaming the parameter or passing it through a generic, trait, or indirect call
cannot weaken the promise. Temporary local views are allowed while their access
remains within the call. Returning a computed scalar does not retain access.

To return or store a retained view, take an ordinary owned input (including an
ordinary `&T` or `&[T]` input), or use explicit lawful `.owned()` materialization
to establish an independent value first. A materialized value still obeys region
escape and resource rules. Normal mode does not silently turn a `let` parameter
into an escaping owner. This keeps its nonescape promise usable by opt callers
and separately compiled code without changing its meaning by caller mode.

```text
// Accepted target: ordinary input permits retention of its snapshot.
fn first(xs: List(Int)) -> &Int:
    &xs[0]

// Accepted target: explicit materialization establishes an independent value.
fn copied_first(let xs: List(Int)) -> &Int:
    let independent = xs.owned()
    &independent[0]

// Rejected: the returned view retains access borrowed for this call only.
fn escaping_first(let xs: List(Int)) -> &Int:
    &xs[0]
```

The examples assume a nonempty list; ordinary bounds failures still apply.
An optimizer may elide physical materialization only while preserving the
independent value and the caller's nonescape contract. For `let view: &T`, the
borrowed object is the carrier: an explicit copy of a copyable referent can
escape, but an implicit retained copy of that carrier cannot.

### Transfers and write-back

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

### Views and noncopyable storage

An unrestricted retained snapshot view requires a backing value whose observed
data can be preserved without duplicating a resource or disposition obligation.
This amendment does not introduce persistent views of `must` values,
once-callables, capability authority, or aggregates whose retained storage owns
such contents. Those forms are rejected in both modes. Call-scoped `let` access
remains available under the payload's ordinary authority and consumption rules;
it cannot consume a borrowed resource or escape as a retained view.

Eligibility follows instantiated stored fields and variants recursively, not
just the selected field, a container name, or every syntactic type argument.
An unused phantom argument does not store a resource. A callback signature
mentioning a capability is not itself a captured capability. Copyable ordinary
callbacks retain their declared invocation contract; an immutable view does
not make an effectful callback pure or snapshot the external state it accesses.

```text
must type Ticket:
    Ticket(Int)

// Accepted target: borrow the ticket to inspect it without taking its obligation.
fn inspect(let ticket: Ticket) -> Int:
    match ticket:
        Ticket(n) -> n

// Rejected: a persistent view cannot outlive consumption of this obligation.
fn retain_ticket(let ticket: Ticket) -> &Ticket:
    &ticket

// Also rejected with an owned parameter; removing `let` does not make Ticket
// eligible for persistent views or discharge the callee's obligation.
fn retain_owned_ticket(own ticket: Ticket) -> &Ticket:
    &ticket
```

`unique` data ownership remains distinct from resource noncopyability. The
existing `&unique T` rule allows observation of otherwise snapshot-capable
data; it never duplicates a unique owning handle. An independent data copy
must establish its own ownership proof. While a view observes the old data,
normal mutation may detach only when that is lawful, and opt mutation requires
proof or explicit materialization. No rule permits copying a unique wrapper to
duplicate noncopyable contents. Similarly, `frozen` never repairs ineligible
resource storage.

### Removed borrow spellings

`View(T, 'a)`, lifetime parameters, `&'a T`, `&'a mut T`, `&mut place`,
write-through `*reference`, `let('a) T`, lifetime-bearing `let('a) x: T`, and
`var('a) x: T` are removed, not aliases. A migration command may parse historical
input in a separate migration-only grammar; ordinary `check`, build, formatter,
and generated-code admission reject it after cutover. Migration must distinguish
immutable views from mutation/write-back and rewrite the latter to `var`; no
historical mutable-reference spelling may silently become a view.

## 6. Normal and opt interoperation

The additional opt-mode and cross-mode rules are deferred in
[RFC-0173](0173-opt-mode-contract.md). Normal mode keeps the same snapshot,
`own`, `var`, `must`, and rooted-view semantics even when it calls an admitted
opt export or uses an opt-defined type. An unsupported or unauthenticated
boundary refuses rather than erasing those obligations. RFC-0173 is not a
current normal-mode acceptance gate.

## 7. Resources, effects, and suspension

`must type T` makes disposition an obligation. No live instance may be dropped,
overwritten, or discarded on a continuing/structured-exit path. Transfer moves
the obligation; borrowing leaves it with its owner. Both success and error-value
returns from a consuming operation must account for it. Unconditional program
abort is not an implicit cleanup protocol. `must` propagates through aggregates
and admitted suspension frames; callable capture restrictions are specified
below. No implicit destructor or finalizer is added.

Recursive stored-resource classification must be combined with control-flow
transfer and checked consuming operations. For a must-bearing `Ticket`,
discarding the possible error of `Result(Int, Ticket)` with `?? 0` is rejected.
For `Result(Ticket, String)`, fallback can instead transfer the success Ticket
or construct one on the error path while dropping only the String; the resulting
Ticket still needs disposition. Phantom arguments and callable signatures alone
do not store their mentioned resources. A nominal must value does not become
discardable because its fields happen to be empty. Any discharge based on
absence of a structural payload needs a checked rule, not a generic erasure or
an assumed exemption for an empty container.

Transfer into deferred work preserves the obligation; it does not prove that
the work ran. Converting an owner into raw state, a callback, a task, an error,
or a generic wrapper cannot make a pending obligation disappear. Require checked
storage and consuming/disposition paths or reject the operation. An outer must
wrapper does not by itself authorize resource captures in an opaque callable.

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

### Task capture, preparation, and execution

Constructing a lazy task evaluates source arguments once in written order and
captures their then-current values or view snapshots. An `own` argument consumes
its source binding at construction, not at the first await. The async body and
its effects run only when driven. Replaying a recipe does not re-evaluate the
original argument expressions or observe subsequent rebinding of those inputs.
Each execution receives its own logical invocation state; it cannot consume or
mutate the reusable recipe's captures on behalf of a later execution.

Capture preparation and execution preparation are distinct. A normal constructor
may prepare copyable captures lawfully, but a once-established unique root cannot
be promised to every replay. Preconditions needed by the body must hold for each
execution. If executing a normal recipe calls opt code, any necessary normal-side
repair occurs at that call within each execution and belongs to its cost. An opt
recipe may not hide such repair in a generated resume or replay adapter; it must
establish the contract without implicit repair or use explicit materialization.
Replayable recipes cannot capture affine unique owners, once-callables, or `must`
values. Copyable owned data and eligible immutable views remain valid captures.

The construction site's source mode cannot change the async body's declaration
mode. Conversely, awaiting a normal recipe from opt code cannot upgrade its
execution contract. Opt execution requires checked opt-compatible task execution
metadata, preserved through aggregates, generics, trait returns, and scheduler
adapters. A task result type alone does not establish this property. This extends
the existing transitive callable rule to deferred execution without adding a
surface effect qualifier or granting a scheduler blanket authority to erase it.

The same rule covers generated resume and cancellation/disposition paths.
Normal orchestration may drive opt work and perform normal work in its own
body; it cannot delegate an arbitrary normal callback to an opt operation by
placing it behind a scheduler or cleanup adapter. Unrelated normal tasks on the
same cooperative VM do not change an opt body's declaration mode.

```text
// Accepted target: replay observes the captured snapshot twice, producing 22.
async fn size(items: &List(Int)) -> Int:
    (*items).length()

async fn demo() -> Int:
    var items = [1, 2]
    let recipe = size(&items)
    items.push(3)
    let first = recipe.await
    let second = recipe.await
    first * 10 + second

// Rejected: a replayable recipe cannot own this affine capture.
async fn affine_capture(own items: unique List(Int)) -> Int:
    items.length()

// Rejected: construction would store call-scoped access past that call.
async fn borrowed_capture(let items: List(Int)) -> Int:
    items.length()
```

These are target examples. A normal caller may instead pass an ordinary copyable
value; a generic wrapper must preserve the same capture eligibility. Cancellation
of an execution releases ordinary retained data, but is not permission to discard
an outstanding `must` obligation. A resource created inside the body must be
consumed, transferred, or covered by a checked cancellation disposition path at
every suspension where cancellation can occur. Otherwise that suspension is
rejected in both modes. An error result or an unexecuted cleanup callback does
not by itself discharge an obligation. This selects no new cleanup syntax or
single-use task type.

Cancellation safety does not promise termination. A task may fail to observe a
request or wait on a host operation that cannot be cancelled. Nor does an error
from cleanup authorize abandoning other live obligations. A recoverable deadlock
or cancellation result must preserve or discharge outstanding frame, child-result,
and message obligations; destroying them and returning an ordinary error is
invalid. Whole-program abort is a distinct boundary with no implicit language
cleanup guarantee. These constraints apply in both modes and across their call
boundary, independently of the future cancellation API selected in section 12.

### Scheduling and suspension boundaries

The proposed RFC-0161 changes to must-start Task values, scoped Child types,
explicit channel-owner close, and selectable-operation syntax are not selected
by this consolidation. They remain a separate deferred surface decision; their
implementation/evidence ledger is preserved. Adopting them must amend this
section first. Typed-runtime representation work that preserves the selected
semantics remains independent. This makes the quiescence choice explicit rather
than treating both closure models as simultaneously normative. The lifecycle
review in section 12 is required before such an amendment; it is not permission
to implement a second Task meaning behind the unchanged public contract.

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

The same obligation applies when an iterator is abandoned or an adapter stops
early. A resource-bearing frame must remain obligation-visible and have a checked
disposition or transfer path; the yielded element type alone cannot erase it.
A synchronous consumer cannot silently acquire an await or arbitrary cleanup
effects. This selects no new close method, asynchronous iterator syntax, or
implicit destructor. A resource composition lacking an admitted disposition
protocol is rejected; ordinary data iterators do not thereby become must-bearing.

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

A `comptime:` item is a parameterless compile-time block. It can bind local
values and call `comptime fn` helpers with arguments; it cannot capture a local
value from a runtime function. `type_info(T)` is a compiler-known, compile-time-
only expression whose operand is resolved in the type namespace, never
constructed or evaluated as a runtime value. For an accessible declared nominal
`T`, it returns `meta.TypeInfo` with its record fields or sum variants and
resolved type expressions. The compiler binds that data to the authenticated
definition identity separately from its forgeable display name. Qualified
names retain their defining module identity. An alias resolves to its target;
a generic instance substitutes its concrete arguments in reflected field
and variant types. The same descriptor rules apply to types declared in normal
and opt modules. An unresolved, inaccessible, or unsupported type is diagnosed
before compile-time execution. `quote type: T` is typed syntax naming `T`; it
is not a structural `TypeInfo` lookup.

Compile-time type lookup observes checked source declarations and earlier
emitted items, never output from a later block. It cannot inspect runtime
values, reveal inaccessible fields, forge a nominal identity, or grant authority
through reflected metadata. A compiler-issued descriptor is data for code generation, not an unchecked source
of ownership, capability, or opt-mode proofs. Generated declarations enter the
ordinary destination checker under their actual source mode; using an opt-
defined type in a normal module does not turn its generated functions into opt
functions. Normal calls into generated opt behavior still need the usual opt
contract and ownership proofs. Type lookup, generated item identity, and
source provenance are bound to the admitted compile-time recipe so a caller
cannot replace the type or its fields after checking.

Useful generation is part of the contract, not only fixed quoted declarations.
For example, a program-defined `builder_items(meta.TypeInfo) -> List(meta.ItemSyntax)`
may receive `type_info(User)` and emit `UserBuilder` plus its inherent methods:
one optional slot and one setter for each accessible `User` field, and a `build`
method that constructs `User` only when every slot is present. These methods
consume the builder's current state when opt mode would otherwise need a hidden
copy. A must-bearing field cannot be silently discarded by an incomplete build.
Typed item construction must support the record declaration and inherent
implementation needed for this example; neither may be assembled from source
strings. Changing
`User`'s fields changes the generated builder after rechecking, while a
missing or incompatible field remains a normal source error. The acceptance
ledger records the concrete normal, opt, and cross-mode cases. The target
source shape is:

```witchy
import meta

type User:
    name: String
    age: Int

comptime:
    for item in builder_items(type_info(User)):
        emit_item(item)
```

`builder_items` is a program-defined helper. This example specifies the target;
the current compiler does not yet implement the whole program.

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
`Hash`, which refines `Eq`. `Hash.hash(let self) -> Int` is a deterministic
content hash: equal keys must return equal hashes, and a key's hash and equality
must remain stable while it is stored. Collisions are resolved with `Eq`.
User-defined keys, including records with `derive(Eq)`, need a matching explicit
`Hash` implementation; the compiler rejects a key without one. Functions and capabilities are not comparable;
Float does not satisfy the `Eq` or `Hash` key contract. Display uses interpolation or `Show`, not a private
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

## 11. Composition clarification follow-through

The 2026-09-25 and 2026-09-26 clarifications are accepted target rules, with
implementation and semantic evidence pending. Existing broad row labels do not
establish these additional cases. Keep changes to the implementation and
current-behavior spec separate from this decision record until their behavior is
established.

| Contract | Required representative acceptance and refusal | Ledger rows |
|---|---|---|
| Call-scoped access | Return a view from an ordinary input or independent materialization; reject borrowed access escaping through a renamed value, wrapper, or callback. | G, J, P |
| Resource-view eligibility | Permit ordinary data views and nonescaping resource inspection; reject retained roots containing a resource obligation, including through generic stored fields. | G, K |
| Mode boundary | Preserve an existing snapshot across normal-to-opt `var` repair; reject the corresponding opt call without proof or explicit materialization. | I, P |
| Higher-order boundary | Admit an opt-compatible callback from a normal caller; reject incompatible provenance through direct, generic, trait, and indirect interfaces. | I, J |
| Deferred execution | Replay a captured ordinary snapshot independently; reject affine or borrowed recipe captures and loss of execution-mode metadata. | J, K |
| Cancellation | Release ordinary view roots; require disposition for every live `must` obligation or reject the suspension. | K |
| Nested ownership preparation | Normal callers establish the actual mutated child's preconditions while preserving old snapshots; opt callers supply proof or explicit materialization. Reject root-only proofs for shared children. | E, H, I, P |
| Imported types | Normal use of an opt-defined plain record retains normal snapshot semantics; unique, frozen, must, view, sealed, and layout contracts survive import. Calling a method uses its declared mode. | E, F, G, I, J, K, L, M |
| Stored behavior | Preserve authenticated callbacks/tasks through admitted constructors, updates, aggregates, generics, and witnesses; reject behavior inferred only from the containing type's module or result type. | I, J, K |
| Disposition through exits | Account for resource-bearing errors, deferred owners, completed results, and abandoned generator remainders; reject any path that loses an obligation. | J, K |

Use shared recursive type/storage judgments and dataflow rules for these
obligations. The table calls for representative semantic witnesses, not separate
compiler implementations or exhaustive concrete-type tests for every nesting.
Neither the table nor this documentation change claims a completed acceptance
row, an allocation bound, or measured performance.

Exercise the applicable cases in normal code, opt code, normal-to-opt calls, and
normal consumers of opt-defined types. Include source-linked boundaries and
separately compiled interfaces where admitted; missing or incompatible metadata
must refuse the affected use. Unsupported carriers remain implementation gaps.
Use representative values, diagnostics, ordered effects, and
ownership/suspension invariants with admitted unoptimized Wasm and focused
boundary checks. No second execution oracle is required.
Optimized execution may corroborate them; it cannot provide missing semantics.

## 12. Deferred resource lifecycle decisions

This section constrains a future amendment. It does not replace the replayable
Task, quiescent closure, or iterator surface selected in section 7. Existing
resource-loss paths still violate the current contract and must be fixed or
rejected; their safety is not deferred with the new APIs.

Before replacing Task, compare a universal owned single-execution Task with a
distinct owned operation alongside replayable recipes. Eligible snapshot replay
is coherent. Affinity alone does not require starting a cold operation whose
captures can lawfully be abandoned. For a proposed resource-owning operation,
its printed result type alone does not establish that its captures can be dropped.
A uniform must-dispose type with checked abandonment and a type exposing
conditional obligations have different generic, export, and usability costs.
Select the public contract explicitly; do not introduce a universal discard
operation or implicit execution.

| Decision | Required answer and adversarial witness |
|---|---|
| Cold operations and factories | Specify capture-time evaluation, abandonment/recovery, and one-time transfer. Compare an unused data-only operation with one owning a Ticket. Show that a factory preserves snapshots and effects and cannot transfer one unique capture repeatedly. |
| Owned execution frames | Specify checked capture storage, activation, suspension, terminal state, and export metadata. Ordinary reusable/once closures cannot hide must captures merely because Task is nominally must-bearing. |
| Structured scopes and join | Account for cold operations, running children, and completed must-bearing results when a sibling fails. Specify handle escape, deterministic exit order, `var` commitment, and pending cleanup's effect on tail calls. Completion-only join can block where quiescent join returned. |
| Cancellation and deadlock | Separate request, observation, cleanup failure, result recovery, and terminal completion. State the behavior of uncooperative tasks and uncancellable host operations. A recoverable failure cannot erase suspended obligations. |
| Channels and selection | Separate admission-close from final queue disposal. Account for buffered resources with no receiver. Define reserve/commit/recovery so exactly one of delivery or payload recovery occurs, with no duplicated argument effects or automatic resurrection of a consumed binding. Choose send conventions for copyable and affine payloads explicitly. |
| Resource generators | Specify ownership of the remainder on early exit, adapter abandonment, and cleanup requiring suspension. Select synchronous/asynchronous disposition explicitly without imposing resource obligations on ordinary data iterators. |
| Modes, types, and metadata | Preserve the section 6 boundary through callbacks, methods, traits, generics, and generated cleanup. Include a normal callback stored in an opt-defined type and a normal factory returning opt work; neither gains opt provenance from its container/result. |
| Migration and evidence | Account for changed collection APIs, closure captures, retained-view eligibility, error propagation, and ABI. Universal must Tasks propagate obligations into ordinary aggregates. Compare representative normal and opt programs before claiming the usability goals are met. |

These answers must agree across all four contexts in section 11. Test the selected
semantics against asserted results and admitted unoptimized Wasm;
test ownership and authority at their checked boundaries. Performance claims
require measurements including normal-side preparation, allocation, and copies.
Source ownership alone proves neither allocation freedom nor a speedup.

The current resource-root view restriction and transitive opt callback rule
remain selected. Their usability costs require representative library examples:
data inside a resource-bearing root may need separate storage or explicit lawful
materialization, and moving callback factories to opt modules may require moving
their dependencies too. Relaxing either rule or adding a local opt-callable
spelling requires an explicit amendment with preserved ownership and provenance.
Independently valid implementation of the current contract can proceed while
these additional language choices are evaluated.
