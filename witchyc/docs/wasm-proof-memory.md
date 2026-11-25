# Proof-required generic memory

Note: the sections on Vec below predate sealed types and type invariants. `Vec`
now states its layout as an invariant, headers cannot be forged, and the
run-time header checks and `vec_well_formed` described here are gone. See
"Type invariants" in `wasm-vec-growth.md`.

The memory foundation supports two CopyStorage operations:
`memory.get_unchecked(&buffer, index) -> T` and
`memory.init_unchecked(&mut buffer, index, item) -> ()`.
Their authors provide Witchy contracts and `proof: auto`. The compiler generates
obligations from the actual typed body; Lean checks bounds and initialized or
empty cells, actual values and declared postconditions. Missing proofs and
unsupported contexts refuse, including selected unused definitions/dead code.
Users do not write companion Lean or register container/type names.

## Initialization and ownership footprint

The substantive initialization clauses in `library/wasm-vec.wy` are:

```text
fn memory_init_unchecked<T>(buffer: &mut Buffer<T>, index: U64, item: T):
    requires index < memory.capacity(buffer)
    requires memory.empty(buffer, index)
    ensures memory.cell(buffer, index, item)
    ensures memory.same_allocation(buffer, old(buffer))
    ensures memory.capacity(buffer) == memory.capacity(old(buffer))
    ensures memory.unchanged_except(buffer, old(buffer), index)
    proof:
        auto
    memory.init_unchecked(buffer, index, item)
    return
```

Bounds and Empty are separate requirements: an absent cell outside capacity does
not authorize a write. Source Flow requires an exclusive mutable borrow and a
CopyStorage payload; a user proof cannot grant a missing permission or duplicate
an owner. Lean checks the logical update with the actual supplied item and index,
other-cell preservation and the fixed logical allocation/capacity.
`memory.frame_unchanged(old(buffer))` is refused: it rendered only `frame = frame`,
so it proved no frame property or physical isolation. The borrow checker
and physical footprint translation remain compiler/runtime TCB. A modular
mutable call still havocs cells; a frame-only post supplies no initialization
or unchanged-cell fact to its caller.

The runtime evaluates receiver, index and the complete payload before committing
stores. It directly writes typed cells, sets the corresponding initialization
bit and increments the allocation's full-slot count. It has no selected-index
bounds/Empty test, checked slot-preparation call or normal Err fallback.
Token/type/descriptor validation remains. Bitmap/count/codec correspondence is
trusted and tested; the logical theorem is not a full Wasm refinement proof.

`Vec.push_spare_mut<T>(&mut Vec<T>, item) -> Result<(), T>` checks claimed versus
actual capacity, spare room and Empty occupancy before calling this wrapper.
It advances length on success and returns the item on failure without writing.
Malformed headers, a full allocation or an occupied target return Err. This
operation does not allocate or grow. Ordinary consuming spare push now also
uses proof-required initialization as described below; reserve/growth, take and
cleanup retain checked memory operations. No full Vec or global prefix invariant
is implied.

## Reads and modular state

`memory_get_unchecked<T>` requires bounds and initialized occupancy, ensures the
actual returned cell value, and calls the raw read. The safe `Vec.get<T>` checks
logical length, physical capacity and occupancy before calling it. Holes and
misleading public headers are handled explicitly. No compiler rule names Vec.

The shared proof model tracks actual current cells, immutable function-entry
snapshots and typed U64 field updates. `old(...)` is ensures-only. A mutable
callee's precondition is recorded after all actual arguments; its cells are then
havoced and only its declared, separately checked posts constrain the new state.
An omitted Cell post cannot justify a subsequent initialized read.

The bounded domain has Buffer foci, directly or through a borrowed record (which
may hold several, each with its own key, capacity and cells: `tests/fixtures/pairs/`),
U64 indices/headers and finite branches. Modular mutable calls inside proved
bodies currently support direct mutable Buffer arguments and Unit returns.
Mutable-record/Result-return effect reuse refuses rather than leaving stale
headers. Ordinary callers may call empty-requires safe abstractions after the
whole generic body is proved. Loops, allocation/release/owner replacement inside
these proof bodies, unknown effects and unsupported contexts refuse. Shared
reads and the existing Source Flow ownership checker remain in use.

## Scope and qualification

This is empty-slot initialization of CopyStorage values, not arbitrary overwrite,
unchecked owning payloads, moves, persistent references or complete Vec safety.
Allocation remains fallible and existing cleanup semantics remain. The trusted
boundary includes type/borrow checking, VC completeness, logical-to-physical
mapping, address arithmetic and Wasm emission. Wasmtime sandbox bounds do not
establish source-level object bounds or initialization.

Source/currentness, kernel/axiom auditing, selected-generation lifecycle and
fail-closed publication follow `wasm-production-build.md`. No verified immutable
SDK claim is added. `scripts/test-proof-memory.py --suite get|init` provides the
focused real-proof/Wasmtime checks with explicit scheduled host, compiler,
binding/runtime and new evidence directory. The init client exercises packed
bitmap boundaries, scalar/record values, failure preservation, slot reuse and
initialization followed by a proved read.

## Ordinary Vec push

The ordinary consuming `Vec.push_spare<T>` now directly uses the generic
proof-required initialization primitive, with its own Witchy contract and
`proof: auto`. It has no caller precondition: executable guards check claimed
capacity against the allocation, the actual index bound and the empty slot.
The existing one-owner record model generates bounds/Empty checks for that exact
raw operation and checks the actual returned owner and item. Success increments
length, preserves capacity, stores the supplied item and preserves other cells;
failure returns the original vector, headers, item and cells unchanged.
The raw operation has the same exclusive-borrow and CopyStorage restrictions.

Ordinary `Vec.push` calls this proved spare path both with spare room and after
its existing checked reserve/growth algorithm. Ordinary `Vec.get` already calls
the proved raw read wrapper. The general copy loop and the actual reserve/cleanup ownership bodies are
proved as described below, using checked allocation, Take and release semantics.
`Vec.push` itself carries a contract, proved from the contracts of
`Vec.push_spare` and `Vec.reserve` rather than from their bodies. Success adds
one to the length, keeps `capacity` equal to the allocation's capacity and
stores the item at the old length. Failure returns the item and a vector of the
old length whose first `length` cells equal the input's. That last clause is
stated only when the input's length fits its allocation: a forged header with
`length > capacity` is returned unchanged, and no prefix claim about it is true.
After growth, failure returns the grown allocation, so the contract does not
promise the same allocation or capacity.

There is no whole-Vec invariant theorem.
These contracts do not assert an initialized prefix for arbitrary public headers.
The end-to-end ordinary client exercises both spare and growth paths across
packed bytes, words, booleans and padded records, plus refusal/ownership behavior.

The versioned one-owner record model reuses its primary typed moves, returns and
cell state for unchecked initialization. Its inventory covers unused definitions,
dead branches and operation arguments. Missing proof or unsupported effects
refuse. Empty-requires callee bodies are universally checked, so ordinary callers
may establish each typed site's trivial precondition without importing any heap
facts or postcondition, or claiming the call was reached. If a caller has any
nontrivial record precondition, its existing state/history/dependency rules apply.
This adds no proof authority from a function or container name.

A body may also consume an owner by calling a function with a record contract,
no precondition and a `Result` return. The model then drops the consumed owner,
splits on an unconstrained choice of arm, introduces a fresh owner and payload
for that arm, and assumes exactly the callee's authored clauses for it. Handing
an owner to a callee is not releasing it: `memory.released` of the consumed
allocation holds only if the callee's contract says so, or if the returned owner
is the same allocation and is itself released later. The
callee's own body proof is a separate obligation, and import cycles refuse.
Callees with preconditions are not imported this way.

A consuming function that cannot fail may return its owner directly; its
clauses name it `result`. `Vec.truncate` is one: it keeps the allocation and
capacity and never lengthens the vector. It may stop early on a vector whose
headers do not describe its storage, so it does not promise `length` is reached.

A loop in such a body needs authored invariants over the current locals and
`old` parameters. They are checked on entry and after every path through the
body, and are all that is known at the loop head and after it: each local the
body assigns becomes an arbitrary value of the same shape. Owners are counted
too. A local that owns an allocation on entry must own a live one at each back
edge, anything else the body created must be gone by then, and outer owners
must be untouched. Termination is not proved.

`vec_well_formed` is a borrowed check that returns a Bool, with clauses of the
form `!result || ...`: a true result means capacity matches the allocation,
length fits it, every cell below length holds a value
(`memory.initialized_range`) and every cell from length on is empty. It leaves
the vector unchanged either way. A memory-contract body may loop over a record
behind a shared borrow, since nothing can write through it.

A record contract may also sit on a non-generic function whose allocations all
hold one unsigned type narrower than U64, such as `U8`. Its stored values are
bounded numbers in the proof. U64 elements are not accepted in this form, since
they could not be told apart from lengths and indices.

Record-model bodies may divide and take remainders (the divisor must be shown
nonzero), use `-=` (no underflow must be shown) and narrow with `truncate`. An
`if` whose branches only compute numbers does not split the proof: each local
the branches disagree on becomes one unknown tied to either value by the guard.

Memory contracts accept the same fixed element types, may return U64 (named
`result`), and may call a proved helper that returns a Result: the body is
then checked once per outcome, with only that outcome's clauses known. A
helper that cannot write may be called inside a loop.

With a fixed element type, stored values read through a proved helper are
numbers and can be compared; narrow mutable locals may change in a loop; and
`&&`/`||` are allowed where the right side is a plain comparison. An owning
body may call a helper that only reads through shared borrows and returns a
Bool or U64: nothing changes and the answer is unknown.

Proof variables that no obligation mentions are left out of the theorem, and
each path drops the range facts of calls it did not pass through before its
leaves are searched. Both only shrink what the tactic scans; every leaf is
still kernel checked.

## General migration loop invariants

The ordinary `Vec.reserve` migration calls the generic borrowed
`memory_copy_prefix<T>(&Buffer<T>, &mut Buffer<T>, count)` helper. Its Witchy
`proof: auto` checks the actual raw read and initialization at each loop step.
The source remains unchanged on every return. An Ok result establishes equal,
initialized source/destination prefixes of the requested length; all writes are
below that length. Err preserves the source and allocation identities but may
leave a partially initialized destination. Reserve drains that fresh destination
with existing checked cleanup and returns the original vector on migration error.
No rollback or infallible allocation is assumed.

The source loop states:

```text
while index < count:
    invariant index <= count
    invariant memory.same_prefix(target, source, index)
    invariant memory.unchanged_from(target, old(target), index)
    ...
```

`same_prefix` states both capacity bounds and equal nonempty source/destination
cells below the index. `unchanged_from` preserves cells at or above it against
the function-entry snapshot. Bounds and source occupancy and target Empty are
established before the actual generic unchecked operations. A missing source
cell or occupied target produces a real Err, possibly after earlier copies.

These annotations generate initiation, induction and exit obligations. Mutable
loop-head U64 locals and mutable buffer cells are replaced by fresh universal
state before assuming the invariant for one iteration. Every normal backedge
must reestablish it; exits and early returns prove their actual authored posts.
Old snapshots remain immutable, and readonly source cells, allocation keys and
capacities stay fixed under the restricted effect domain. An invariant is not
assumed at entry or inferred from a Vec/function name. Annotations are proof-only
and erased for execution after the generic body is checked.

This bounded domain supports pure guards, direct borrowed buffers and mutable
U64 loop-head locals. Unsupported effects, nested loops, mutable records and
unproved annotations refuse. It proves normal-return partial correctness, not
termination, host interruption freedom or fuel sufficiency. The compatibility
frame clause is unsupported with multiple focused allocations and refuses in
that domain. This restriction does not turn its single-focus reflexivity into a
physical isolation theorem.

The original sealed same-owner post-reuse rule requires returning the same
allocation. A correct growing Vec returns a different allocation, so that reuse
rule is excluded from the general allocation/lifetime proof below. The copy loop's memory and
source-preservation contract is proved. The later general ownership layer below
also checks the actual reserve/cleanup bodies; their physical allocator mapping
remains trusted and tested.
Full Vec prefix invariants, allocation refinement and general owning-element
migration are still outside the proof claim.

## Ordinary reserve allocation and ownership handoff

`Vec.reserve` now has its own Witchy `proof: auto` body contract. It permits
arbitrary public headers, returning Err for inconsistent capacity/length. On Ok,
length is preserved, capacity matches the actual allocation and meets both the
old and requested capacity, and the logical Option-cell prefix equals the old
prefix. Option equality deliberately includes holes on the no-growth path; it
does not invent a global initialized-prefix invariant. The returned allocation
is either the original one or the original has been released. On Err, the
original allocation, headers and all its cells are preserved.

The general owned model treats allocation as real Ok/Err alternatives. Ok creates
a fresh live allocation with requested capacity and empty cells; Err creates no
owner and does not mutate the input. The model checks all tracked allocation
lifetimes on normal return: only the actual returned owner may remain live, or
none for Unit cleanup. Returning a different allocation does not qualify for the
older sealed same-owner post-reuse rule.

Verified helper calls bind actual arguments before snapshots and import only
the current declared posts. Mutable cells, and consumed owner lifetime where
applicable, are fresh state constrained by those posts. Result arms are checked
separately. Each import requires the current body and exact call-site precondition
in the same kernel batch, with an acyclic import graph. Unsupported reachable
calls refuse; a self/unsupported call can be bypassed only by proving its current
path False, without importing its spec. This handles the post-clear impossible
release-error recursion without circular cleanup assumptions.

Actual `vec_buffer_close` calls general `memory_clear`, whose authored loop
invariant proves all cells empty using checked Take effects, then calls checked
`try_release`. Actual `Vec.close` empties its first `length` cells with
`memory_take_prefix` and proves release of its consumed buffer.
The general `memory_check_empty_from` loop proves the tail is empty before
reserve drops the original allocation. Copy or tail failure closes the fresh
partial allocation and returns the original owner; allocation failure returns
the original directly. These are specs on the executed library bodies, not
separate implementation substitutes.

This is normal-return partial correctness and bounded resource ownership,
not a termination proof or whole-Vec/compiler refinement theorem. SourceFlow
remains permission/alias authority. Allocator freshness, empty-cells/count
correspondence, physical release/layout/codec and VC/emission translation remain
trusted and tested. Concrete Wasmtime checks include real allocation failure and
repeated partial-copy failure beyond the allocator's metadata-slot count, followed
by fresh allocation and original-element access. No name/type certificate or
additional runtime primitive grants proof authority.

## Records with several allocations

A sealed record may hold several Buffers of its element type, each with its own
invariant (`Pairs<T>` in `tests/fixtures/pairs/two-buffers.wy`: keys and values,
`memory.prefix` over each). The memory model gives every Buffer reached through
a borrowed record its own focus, and stores to the record's U64 fields are
modeled whatever the number of foci. The record model treats a record of several
allocations as one owner when it is moved whole; at a call it requires every
allocation the arguments own to hold the call's element type, and a function
returning such a record must leave exactly the returned allocations live and
every other one released. `new` (two allocations, the first released when the
second fails), `push_mut`, two reads and `close` (both released) are proved; five
controls are refused by Lean (a write missing, a write to the wrong cell, a cell
written twice, a false `ensures`, a release of a buffer not emptied).

`grow` (both allocations moved into larger ones, the old ones released) and
`push` (which calls `grow` and continues with what it hands back, then writes
through `push_mut`) are proved too, with four more controls refused by Lean
(`pairs/grow-*.refused.wy`, `pairs/push-*.refused.wy`).

What a call does to several allocations, in the record model:

- A callee that borrows a record mutably leaves every allocation the record owns
  with fresh cells, described only by the callee's clauses (`record_memory.rs`).
  Its answer may be a Bool or a number as well as Unit or a Result.
- A callee that consumes a record consumes every allocation in it. If the arm's
  payload owns nothing, the callee's own closure proof says each was released.
  If it owns some, any consumed one may have come back: each gets a successor
  (the payload's allocation at the same field, if there is one) and a fresh
  claim of release that only the callee's clauses, or a proved equal key, can
  establish. So `released` never holds by default.

Found on the way: `memory_flat` and `memory_predicate` handed a clause neither
handled (a callee's Bool answer) back and forth until the node budget stopped
them, and the model threads had 2 MiB stacks, so the stack ran out first and the
compiler crashed. Each now refuses such a clause, and every model thread has a
fixed 256 MiB stack (address space; only what is used is touched), so the
budget, not the stack, is what stops a large proof (`src/compiler/parallel.rs`).
