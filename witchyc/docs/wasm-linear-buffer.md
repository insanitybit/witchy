# Opaque linear Buffer value-flow prerequisite

> **Historical.** This page records a milestone of 2026-11 and its limits then; most have
> since been lifted. For what holds now see docs/wasm-references.md, docs/wasm-sums.md and
> docs/wasm-unions.md, docs/wasm-generic-functions.md and docs/wasm-generic-records.md,
> docs/wasm-memory-sizes.md, docs/wasm-hashmap.md and library/wasm-vec.wy.

The compiler recognizes concrete `Buffer<T>` as a non-Copy, non-discardable
private-call type. Its opaque physical carrier is i64. This slice has no factory,
allocator, allocation/capacity/release operation, element access, memory storage,
Buffer reference or host grant. Main remains Buffer-free. There is no source
way to obtain an actual Buffer token in main; unused selected helper bodies are
the discriminating type/ownership checks.

```text
fn relay(b: Buffer<U8>) -> Buffer<U8>:
    return move b

fn rebound(b: Buffer<U8>, again: Bool) -> Buffer<U8>:
    var slot = move b
    while again:
        slot = relay(move slot)
    return move slot

fn remap(r: Result<Buffer<U8>, U64>) -> Result<Buffer<U8>, U64>:
    match move r:
        Ok(b):
            return Ok(relay(move b))
        Err(n):
            return Err(n)

fn main():
    return
```

These helpers are checked and emitted with private i64/tagged-register ABIs.
They do not demonstrate executable allocation, opaque-token validation or heap
memory safety. No diagnostic/raw export is added to inject fake Buffer handles.

## Concrete type and representation boundary

Elements must be existing concrete nonunit Copy storage types: scalars, Bool,
concrete records or fixed arrays, with descriptor alignment at most eight.
Buffer, Result, references, Console and owner-containing elements refuse, including
phantom generic ownership arguments. Generic functions/abstract element types
remain unsupported. Source nominal type identity is preserved even when two
record layouts or scalar carriers have equal sizes.

Buffer itself is allowed only as a standalone private owned parameter/result or
local, and at one Result level. Records/arrays/generic record arguments cannot
contain it. Shared/mutable Buffer references and frame addressability refuse.
Checked storage lookup and initialized-cell traversal reject Buffer. The i64
carrier cannot be projected, converted to/from an integer or constructed by source.
`Buffer` is reserved against counterfeit selected type/function/lemma definitions.
Existing main signature rules and an explicit main-value gate prevent Buffer or
Buffer-containing Result values from being introduced through an inactive arm or
recursive private return. No old library declaration is silently activated.

## MUST-consume normal paths

The central type properties distinguish Copy, affine-discardable and linear
non-discardable values. Existing logical slot IDs and Available/Consumed/Maybe
flow are reused; there is no second ownership analysis. Result is non-discardable
when either alternative can carry Buffer. Only the selected match binder acquires
an owner duty; inactive machine zeroes never become owners.

Every possible live linear binding must transfer on every normal scope exit or
early return. The returned expression is evaluated first, so `return move b`
transfers its duty before other visible owners are checked. A returning branch
must independently satisfy its exits before exclusion from continuing joins.
Nested scope restoration cannot revive consumed outer bindings or erase a live
local. Maybe-live obligations remain outstanding and cannot implicitly expire.

Fresh owned rvalues also have duties even without a named caller slot. Ignoring
`relay(move b)` or a linear Result-returning call refuses, as does
`discard(move b)`. Constructors transfer only their active payload; ignoring or
leaking the resulting named sum is not a way to discard the moved allocation.
Nonunit match arms require binders, and a live Buffer binder cannot be ignored.
Existing affine Console discard remains valid and has no heap close callback.

Mutable linear replacement evaluates/checks its RHS first, then requires the
old slot to be definitely Consumed. Thus `slot = relay(move slot)` is valid;
`slot = relay(move other)` cannot erase an Available or Maybe-live old owner.
Failed replacement leaves that old state intact. Affine replacement retains its
explicit discard permission; no arbitrary non-Copy type gains that permission.

Loops retain existing conservative ownership backedge restoration and condition
exit effects. This no-factory slice cannot supply a genuine source primitive
that consumes Buffer and returns Bool. Direct Flow unit controls discriminate
Maybe replacement, skipped/evaluated conditions, zero-body exit state and restored
backedges; they are not presented as source/runtime Buffer condition evidence.

## Receipt and qualification scope

Optional `buffer_types` entries describe `id`, canonical `name`, ownership
`linear`, carrier `i64`, `frame_storage: false`, concrete `element` name and
`element_stride`/`element_alignment` from the existing layout descriptor.
Examples are `Buffer<Pair>`, `Buffer<Duo<U8, U64>>` and `Buffer<[U8; 3]>`.
Descriptors intern by actual concrete type identity; names normalize commas and
array semicolons with a single following space. The existing total 128-descriptor
budget includes aggregates, sums and Buffers. The field is absent without
selected actual Buffer instantiations.

Sum metadata labels Copy alternatives `copyable`, explicitly discardable owners
`affine` and non-discardable owners `linear`; a Buffer-containing Result records
`implicit_discard: false`. Metadata creates no token, allocation identity,
operator authority, heap proof or admission. Existing schemas, source/compiler/
host/module/receipt bindings, memory profiles, grants and opcodes are unchanged.

Source controls separate eligible ownership failures (exact W0401 scope, rvalue,
replacement or moved-state diagnostics) from type/feature refusals. A valid scalar
lemma plus leaking Buffer must fail runtime checking before Lean resolution.
All selected helpers, including unused ones, undergo the same checks.

This is a static ownership prerequisite, not allocator/Buffer execution, release,
destructor cleanup, initialized slots, growth, owner-element/ZST cleanup,
semantic unsafe VCs, generic Vec/String or reusable proof admission. Future
allocation needs separately specified concrete type/token/generation identity,
unchanged failure state, commit boundaries and actual focused runtime evidence.
No allocator behavior is activated from those design notes.
