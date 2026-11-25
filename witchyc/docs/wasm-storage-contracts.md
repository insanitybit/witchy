# Generic storage Init contracts

This slice passed focused Rust checks, independent review and fresh Lean
qualification at 8783d97. One generic body theorem covers the four tested concrete
layouts. Finite runtime branches/matches, record ghost paths and Vec invariants
remain open.

The initial subset accepts a private function with one existing CopyStorage type
parameter, exactly one `&mut Buffer<T>` parameter, at least one T payload parameter,
optional U64 parameters, and result `Result<(), U64>`. Its runtime body must return
`memory.init` directly on that buffer. The actual index is a U64 parameter/literal;
the actual payload is a T parameter. Requires, lets, effects, control flow, other
storage operations and contracted modular storage callers are refused. Ordinary
uncontracted generic/control/ownership code keeps its existing behavior.

```witchy
fn initialize<T>(buffer: &mut Buffer<T>, index: U64, item: T) -> Result<(), U64>:
    ensures match result:
        Ok:
            memory.same_allocation(buffer, old(buffer))
            memory.capacity(buffer) == memory.capacity(old(buffer))
            index < memory.capacity(old(buffer))
            memory.empty(old(buffer), index)
            memory.cell(buffer, index, item)
            memory.unchanged_except(buffer, old(buffer), index)
        Err(code):
            (code == 1 && index >= memory.capacity(old(buffer))) || (code == 2 && index < memory.capacity(old(buffer)) && !memory.empty(old(buffer), index))
            memory.unchanged(buffer, old(buffer))
    proof:
        auto
    return memory.init(buffer, index, item)
```

`old(buffer)` is an entry snapshot, never a source owner copy. Capacity is a
mathematical Nat; Empty is None, and cell(view,index,item) means Some(item) using
logical T equality. Empty outside capacity does not imply valid bounds. Same
allocation compares the entire opaque instance-domain/slot/generation/nominal-type
identity. Unchanged compares identity/capacity/all cells; unchanged_except excludes
only the specified cell. Frame preserves all unrelated allocations, including
allocations with different element layouts. It does not assert a physical stack
frame theorem. These names are clause-only storage predicates, not Vec intrinsics.

Each result match is exhaustive with Unit Ok (no binder) and U64 Err (one binder).
The Err binder is a distinct typed clause role, cannot shadow any parameter/result,
and is visible only in its own arm. Runtime locals cannot capture clause names.
Nested matches/old snapshots, free names, unsupported arithmetic or arbitrary-T
numeric comparisons refuse. Parameters cannot be named result. There must
be ensures and exactly one proof:auto block. Limits are 32 actual predicate leaves
across every arm, 32 parameters, depth64, 4096 slice nodes and 64KiB rendered body.

One proposition per selected definition quantifies arbitrary α, opaque Key and
heterogeneous Frame types, focused cells `Nat → Option α`, entry capacity and every
payload/index parameter. Entry capacity/indices are bounded by U64; cells outside
capacity are None. The trusted valid unique-buffer abstraction also represents
coherent live identity/metadata/counts. Invalid raw tokens or corrupt metadata may
trap and lie outside this normal-return theorem.

The actual typed Init operands define one transition: out-of-bounds returns Err1
and unchanged cells; Full returns Err2 and unchanged cells; Empty returns Ok(Unit)
and updates exactly the actual index with the actual payload. Key/capacity/frame
are preserved by construction. The authored result match consumes that transition's
actual Result; no unconstrained result or assumed authored postcondition is added.
Structural Option cases and Nat-index equality require no DecidableEq/Inhabited α,
encoding, default payload, concrete width or representative instantiation. Inline
case/simplification proofs remain under the existing exact theorem/axiom audit.

Every actual typed occurrence calling a verified/no-requires wrapper receives a
True precondition goal, including nested/control/unused generic caller bodies.
Sites count occurrences in deterministic argument-before-call order per caller;
deduplicated call edges and concrete instance inventories do not invent sites.
Ordinary type/loan/ABI/layout checks finish before Lean. The generic body theorem
does not repeat per concrete layout; concrete instance limits remain mandatory.

Storage goals add `source_model: witchy.checked-storage-init.v1`; body goals also
add `generic_parameters: 1`. Scalar/ghost receipt shapes are unchanged. Model
identity includes the new version only when storage goals exist. Source/compiler/
model/tool/module freshness, exact expected propositions, trust=0, closed theorem
inventory, allowlist, admission_reuse=false and atomic publication are unchanged.

Kernel success establishes the generated abstract proposition. Typed translation,
VC completeness and primitive-to-physical identity/metadata/bitmap/count/codec/Wasm
correspondence remain compiler TCB and need independent runtime/layout controls.
Wrong actual index or same-T payload remains a well-typed theorem failure. This
slice does not complete the Vec body, caller preconditions or reusable admission.

## Lending through a stored weak pointer

A `Weak<T>` stores a Wasm32 address. Its proof projections are
`memory.pointer_bits(link)` and `memory.box_id(link)`. The latter identifies the
allocation event. Reallocating a freed address produces a new event. Given a
loan of an initialized slot, `memory.follow_loan(witness, link)` requires both
projections of the link to agree with the witness's allocation. The result is
a shared loan of the same element, subject to the ordinary borrow checker.

A sealed owner can also describe a recursive collection of initialized slots.
The author writes a shared selector that returns a slot by matching fields
and recursing through owned storage. The checker reads that method's actual
body. A `memory.member(loan, selector)` contract records that the loan designates
a member of that particular root and selector. A caller can carry this fact
through a method's declared `Some` result into the next call.

`memory.lend_enclosing(selector, member, link, focus)` follows a stored link
back towards that root. The link operand must be a fixed weak field of the
actual member. The focus is a shared method on the same owner as the selector.
Its checked postcondition must relate the child's stored field to its enclosing
initialized slot in both address and allocation event. The walk must end in
that same slot projection on every terminal arm. The operation also needs
proof of membership, a nonnull input link and a null link at the outer focus.
The generated `enclosing_member` theorem uses these facts to identify an
earlier live slot on the member's finite owning path. Wasm evaluates the weak
field operand to produce the shared reference. The proof operands are erased.

Witchy checks linear ownership and loan conflicts. Lean checks the generated
body, caller and path obligations. Their interpretation as live physical
storage depends on the typed projection translation, allocator and emitter.
The list library's `first`, `focus`, `front` and `next` methods exercise these
rules. The client traverses twice, ends its loans and removes a node.
[Library](../library/wasm-cell-list.wy) · [Client](../examples/node-links/composed-forward.wy)
