# Rooted views and opt mode

Most Witchy code uses owned values and the ordinary `let`, `var`, `own`, and
`move` vocabulary. Both source modes provide immutable rooted views for code
that needs to read existing storage without copying it:

```witchy-static
mode opt
```

`mode opt` is a file-level source contract for stricter ownership and
no-hidden-copy proofs; it does not change the meaning of a view. A view is a
value-semantic snapshot handle: its carrier can be copied, returned, stored,
passed through a generic, or captured by a callable. Copying the carrier does
not copy or consume its referent. A view is not a mutable access permission and
does not expose an address.

## The two view types

Witchy has a view of one value and a view of a contiguous range:

```witchy-static
&T
&[T]
```

The expression `&place` captures the current value or projection as a view:

```witchy-static
var values = [1, 2, 3, 4]
let whole = &values
let prefix = &values[0..2]
```

The expression is bounds-checked when it forms a slice. Empty slices are valid,
and slicing a slice composes the authenticated projection. A field or element
view keeps the actual root and projection, not a copied field name or index.

Views are read-only. These examples describe the source contract and are kept
in `witchy-static` blocks because carrier-heavy reference APIs are not yet a
general runnable-book surface:

```witchy-static
fn first[a](items: &[a]) -> Option(&a):
    if items.length() == 0:
        None
    else:
        Some(&items[0])

fn show(let text: &String) -> Int:
    text.length()
```

There is no separate view wrapper type to construct. The `&T` or `&[T]` type
and the `&place` expression carry the root and projection together.

## Bindings and snapshots

`let` makes a binding immutable. `var` permits local rebinding and, for a
`var` parameter, writes the final value back to the caller's slot. Neither word
changes the meaning of a view:

```witchy-static
var values = [1, 2, 3]
let before = &values[0..2]
values[0] = 9
```

`before` remains the snapshot `[1, 2]`. If a mutation would change storage
observed by a live view, the implementation uses copy-on-write (CoW). The
mutation may therefore detach the `var` value; it never retargets, invalidates,
or mutates an existing view. A view formed from a later value observes that
later snapshot. In `mode opt`, a direct or function-value `var unique List(Int)` call is refused after a view of that argument root has been formed: preserving the view would require a hidden copy. The current check conservatively remembers viewed roots for the function, so it can also refuse a call after a view is no longer used. Without a preceding view, the unique call writes back to its original root.

`var` is the sole mutation and write-back syntax in this model:

```witchy-static
fn increment(var value: Int):
    value = value + 1
```

A view cannot be used to assign to its referent. To make an independent owned
value, use the ordinary owned-value operation at an API boundary that requires
one:

```witchy-static
fn copy_for_storage(let view: &String) -> String:
    view.owned()
```

For a slice view, `view.owned()` copies the viewed range into a fresh
`List(T)` when the checked element type has a proven recursive layout and
retain, clone, and release behavior. This includes supported scalar and String
elements and `List` elements at any supported depth. A generic `&[a]` can use
this operation after specialization proves `a` has that carrier. The method
reads the retained snapshot, even if the original list was changed after the
view formed. Unsupported element types are rejected. A declared `owned`
method takes precedence over this compiler-known operation.

## Ownership, `own`, `unique`, and `frozen`

The view carrier retains its backing root while any view copy can still reach
it. `own` consumes that view carrier; it does not consume or mutate the viewed
payload:

```witchy-static
fn keep(own view: &String) -> &String:
    view
```

An owned value that contains a view retains the view's root as part of its
value. A view can therefore be returned or stored in an aggregate, subject to
the ordinary ownership rules for that aggregate.

`unique T` describes uniquely owned root storage. It can enable in-place
mutation when no view keeps that snapshot live. Forming a view preserves the
snapshot but means a later `var` mutation must detach when necessary. Coercing
unique storage to ordinary `T` consumes the unique handle; an untracked alias
does not establish uniqueness.

`frozen T` describes deeply immutable owned payload. Moving or replacing a
frozen value is legal, but changing a field or element is not. A frozen shell
may contain immutable views; freezing does not alter the view's root or grant
any write authority. `unique frozen T` and `frozen unique T` are rejected.
Qualifier placement matters for views: `unique &T` and `frozen &T` are rejected
because a view is already immutable and is not an owned storage class, while
`&unique T` and `&frozen T` are legal views of those payload types.

## Returning, storing, and generic transport

Views preserve their authenticated root and exact projection across ordinary
value boundaries. The following signatures show the intended contracts:

```witchy-static
type Window:
    body: &String
    start: Int

fn window(let text: &String, let start: Int) -> Window:
    Window { body: text, start: start }

fn identity[a](let value: &a) -> &a:
    value

fn head[a](let values: &[a]) -> Option(&a):
    if values.length() == 0:
        None
    else:
        Some(&values[0])
```

These are explanatory `witchy-static` examples: they specify the carrier
contract without promising that every nominal aggregate or generic view
surface is accepted by the current frontend. Where a carrier is not yet
admitted, the compiler must reject it rather than silently materialize an
unrelated value or reconstruct a place from source syntax.

One admitted nested whole-root case is `&List(List(Int))`. It keeps both the
outer list and its owned child lists alive while the original `var` list is
changed. The compiler also recognizes exact inner `Float`, `Bool`, `Duration`,
and `String` lists, though those cases await semantic and leak evidence. A
qualified child such as `&List(unique List(Int))` is refused, and a region-born
list cannot escape by wrapping it in a view.
The inner `List(String)` owns its String children; an exact slice of these
inner lists also has a separate carrier, pending semantic and leak evidence.

Slices of owned lists use the same recursive carrier at every supported depth.
The outer list remains rooted, and each managed child is retained through an
owned pointer and a retained List header. A String leaf owns its byte buffer.
Nonempty tuple elements use an owned packed record, with the same ownership
proof applied to every field. Dictionary elements use two owned, retained
lists for their keys and values, with the same proof applied to each element.
A declared one-variant heap record is admitted when every instantiated field
has that proof and an owned physical slot. Tagged, packed, must-consume,
capability-bearing, and recursive records remain unsupported as slice elements.
An untagged record may also hold one direct `&[T]` field when `T` has that
proof. Nested slice fields and tagged wrappers remain unsupported.
Carrier proof is bounded at 128 nested levels
and 4,096 type nodes.
Qualified children and elements without a complete ownership proof are
refused. The exact previously evidenced cases remain recorded in the RFC-0167
ledger; newly generic depths await semantic and leak evidence. `.owned()`
materializes the checked range as a fresh `List(T)`.

Whole-root tuple views use a recursive checked ownership proof for every
field. The view retains the tuple root and its managed children. The original
`&(Int, String)` case has admitted Wasm and zero-leak evidence; newly generic
field combinations remain implemented but unverified. Qualified or
view-valued fields without a carrier, and closure capture of tuple views,
remain refused.

Eligible non-packed `Option`, `Result`, and declared user aggregate roots can
carry an exact direct `List(S)` child for unqualified `S = Int`, `Float`,
`Bool`, `Duration`, or `String`; `&Option(List(Int))` is one example. The outer
view retains its owned child. Qualified or nested List elements are refused;
these new cases await admitted Wasm and leak evidence.

The same rule applies to destructuring, field access, indexing, iteration,
`Option`, `Result`, tuples, and supported nested aggregates: transport the
carrier and its proof, or reject the operation. A displayed field name or
integer index is never enough to recreate a view after a call or return.

## Callable contracts

Reference kind and view placement are part of callable identity. A direct
function, method, function value, closure, trait witness, generated adapter,
or tail-dispatched call must preserve the complete carrier contract:

```witchy-static
fn inspect(let value: &String) -> Int:
    value.length()

fn apply[a](let f: fn(&a) -> Int, let value: &a) -> Int:
    f(value)
```

An adapter that drops the root, changes the projection, turns a read-only view
into writable access, or changes ownership convention is rejected. A view may
be captured by a callable only when that callable's capture is admitted as
non-escaping. It may not be serialized as an address, sent through a channel,
held across suspension, or erased into an owned existential without an
explicit materialization.

## Normal and opt mode use one ABI

Normal-mode code sees value-oriented signatures. An opt module may publish
those same signatures and use rooted views internally:

```witchy-static
mode opt

pub fn inspect(let text: String) -> Int:
    text.length()

pub fn normalize(var text: String):
    text = text.trim().to_lower()

pub fn digest(own text: String) -> Digest:
    digest_string(text)
```

The boundary is canonical: normal callers use ordinary `let`, `var`, and `own`
conventions, and the compiler supplies a copy, re-own, or write-back adapter
when the owned contract requires it. There is one callable identity and one
value ABI, not a hand-maintained normal/opt overload pair. A normal import may
call an opt export whose public contract contains a rooted view. The exact
authenticated root-plus-projection carrier remains in that signature.

Conversely, an opt caller must satisfy the declared contract directly. A
separately compiled ABI may fail closed when its metadata or carrier support is
unavailable; it may not silently detach or erase the view. A normal value
returned after an opt call remains logically owned and independent of any
source view. If sharing remains in its representation, the compiler detaches
before mutation, escape, serialization, host return, or another boundary that
could observe sharing.

## Runtime and optimization meaning

The language contract requires an authenticated backing root, an authenticated
projection, root retention, read-only access, and value semantics at every
source and ABI boundary. It does not require one physical representation.

The source tree walker is frozen for this contract and provides no runtime
semantics. The canonical SafeWIR interpreter is the semantic reference for
admitted programs. Its carrier records the root and projection and checks
every read, slice bound, copy, consume, return, aggregate transport, and CoW
detach.

Wasm may use a typed carrier, a direct place descriptor, a cell-backed carrier,
or another representation admitted by SafeWIR. The choice is an optimization,
not a source-visible distinction. Every admitted Wasm path must agree with the
canonical interpreter on values, snapshots, root retention, `own` consumption,
`var` write-back, traps, and rejection boundaries. Parity evidence is required
for the canonical interpreter and the admitted Wasm artifact; structural Wasm
validation alone is not semantic evidence.

This freedom includes copying: a backend may choose a forced-copy path when a
view carrier cannot be retained safely. It may not use that freedom to erase a
projection, widen authority, or make a stale snapshot observe later mutation.

## Diagnostics and mode boundary

When a rooted-view operation is not admitted, diagnostics should identify the
unsupported carrier, root or projection obligation, and the operation that
would lose it. The repair is usually to keep the view in an admitted carrier,
pass an owned value, or materialize explicitly with `.owned()`.

For example, `Dict(String, &String)` currently fails canonical admission with a
missing collection-carrier diagnostic, including when nested in a record.
Store an owned `String` value if the dictionary must execute today.

Whole `&Dict(K, V)` views have a retained-root carrier when both element types
pass the recursive owned-list proof and SafeWIR validates their physical
slots. Existing String-key scalar witnesses retain their snapshots after
owner updates. Newly generic combinations such as `&Dict(String, List(Int))`
remain implemented but unverified. Qualified or view-valued children without
an ownership proof, region-born root escape, and dictionary-view closure
capture remain refused.

Whole tuple views likewise retain their old root after the owner is replaced.
The proven `&(Int, String)` case covers direct calls and returns; newly generic
field combinations await durable semantic and leak evidence. Region-born root
escape and tuple-view closure capture remain refused.

Normal source may use the same view syntax and call view-bearing exports. When
a separately compiled boundary lacks authenticated carrier metadata, it fails
closed at that boundary rather than changing the signature or silently
materializing a value. Internal root, carrier, projection-proof, and backend
layout details remain implementation details in either source mode.
