# Modular typed calls and proof obligations

The active architecture checks each selected generic definition once in the
primary typed body, then emits concrete instances for supported layouts. Each
source call checks its signature, actual arguments, loans, moves and result
shape. Unused definitions and unreachable source are checked; instantiation
counts do not replace body checks. Physical lowering consumes this checked body
without repeating ownership analysis. See [semantic body](wasm-semantic-body.md)
and [generic functions](wasm-generic-functions.md).

| Current boundary | Evidence required |
| --- | --- |
| Ordinary calls | Source type/ownership/capability checks and actual emitted-instance behavior. |
| Contracted body | One generic obligation over actual typed control/dataflow, under authored requires. |
| Contracted caller | Every actual typed call site establishes its complete requires from current state/path. |
| Owned post reuse | Fresh body and predecessor preconditions, sealed actual owner/effect history, branch-scoped supported post facts. |
| Proof-required memory | Read bounds/initialized and initialization bounds/Empty; actual generic values, exclusive writes and before/after cell footprints. |
| Checked extraction | Actual Take returns the old selected value and clears only that cell; bounds/empty failures retain state. Checked subtraction needs a separate no-underflow body obligation. |

## Source proof composition

Users write Witchy contracts and `proof: auto`; generated Lean is compiler
machinery. [Typed named fragments](wasm-spec-predicates.md) expand hygienically
before these existing proof models; their declarations are not assumptions.
Generic record-body proofs do not imply arbitrary caller headers
are valid. Caller proofs track actual allocation/initialization history and
stable typed call-site identities, including selected unused/dead contexts.
There is no frame clause: `memory.frame_unchanged` stated only `frame = frame` and
was removed (2026-11-15). Nothing is claimed about allocations a contract does not
name, and cells havoced by a mutable call are recovered only by actual checked
cell/footprint postconditions.
All actual arguments evaluate in order. Unknown effects or unsupported contexts
refuse rather than manufacture a successful precondition.

The bounded owned-return slice supports a second push in the first Ok arm.
A descriptor is sealed from actual typed returns and modeled effects, not from
an authored same-allocation claim. Current body/predecessor obligations must all
be checked in the same batch before supported Ok-arm post facts justify a later
call. Old snapshots and current fields/cells remain distinct. Err-arm post
reuse requires an explicit eligible returned-prefix postcondition; without it,
the returned state remains unknown and later nontrivial calls refuse. Unsupported
helpers and histories also refuse. Source Flow remains the sole ownership
authority; symbolic state never grants an extra owner. This modular VC rule is
compiler TCB, not an executable refinement theorem. See [caller and owned-post
scope](wasm-vec-caller-proof.md).

## Readonly memory foundation

The generic proof-required getter takes a shared Buffer borrow and returns a
CopyStorage value, with no owner transfer. Its author proves actual bounds and
initialization; a safe borrowed-record getter checks logical length, actual
capacity and occupancy before calling it. The result denotes the actual stored
slot. Empty-requires safe abstraction calls need no invented caller heap facts;
nontrivial requires need a supported explicit proof context. Missing proofs,
unproved nontrivial calls and unsupported effects fail closed. See
[proof-required memory](wasm-proof-memory.md).

The proof-required `vec_push_spare_mut` writes an Empty slot through a mutable
borrow and advances length after establishing bounds and occupancy. Modular
mutable Buffer calls havoc cells and import only separately verified posts;
mutable-record effect reuse currently refuses to prevent stale header facts.
Ordinary consuming `vec_push_spare` also proves its direct raw initialization,
actual success owner/cell/length update and unchanged failure owner/item. Its
empty requires allow normal `vec_push` callers without invented heap facts;
nontrivial requirements retain their existing caller-history rules.
Existing checked Vec growth and UTF-8 String are executable ordinary source,
not automatically proved invariant-preserving libraries. The ordinary reserve
copy loop now uses a general two-buffer helper with authored invariants and
checked initiation/preservation/exit obligations. It proves copied prefix and
source preservation, with honest partial-destination Err outcomes; allocation
handoff and cleanup now have separate actual-body ownership obligations.
Reserve models real allocation errors, fresh owners, current verified helper
posts and all tracked lifetimes on return. It preserves an Option-cell prefix
on success and original owner/headers/cells on failure. Same-owner reuse does
not apply to allocating bodies. All imported body/site proofs must be current
and their dependency graph acyclic. General loops,
arbitrary owning payloads, persistent references, unchecked moves/arbitrary overwrite and
complete compiler/allocator refinement are outside this bounded proof scope.

The [generic Stack example](../examples/generic-proof-stack.wy) exercises these
same rules through fixed-capacity push/pop and cleanup, including malformed
public headers. Its body contracts state actual item/length/footprint and
failure preservation; they do not establish a whole-client sequence theorem.
The focused `scripts/check-generic-proof-stack.py` runner checks current source
proofs and Wasmtime output, independent checked-Take outcomes, and changed-proof
or ownership refusals with preservation of the previous valid output.

## Validation and lifecycle

Validate a changed body/caller pair on its actual source, emitted layouts,
invalid/unused claims, proof-erased bytes where meaningful, runtime outcomes and
failed-publication preservation. Separate type rejection, kernel proof success,
physical execution and performance evidence. Use focused checks before broader
matrices; do not run a slow historical gate for each edit.

Fresh and operator-selected checker modes retain their respective currentness
contracts. Resident body retention still kernel-checks every current application;
healthy budget renewal starts an empty checker and cannot retry a failed proof.
[Production builds](wasm-production-build.md) documents these rules.
Historical native call experiments and qualification tables are available with
`git show 0731e426:docs/modular-calls.md`; they are not active compatibility promises.
