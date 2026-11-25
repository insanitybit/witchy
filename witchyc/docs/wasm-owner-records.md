# One-owner records

> **Historical.** This page records a milestone of 2026-11 and its limits then; most have
> since been lifted. For what holds now see docs/wasm-references.md, docs/wasm-sums.md and
> docs/wasm-unions.md, docs/wasm-generic-functions.md and docs/wasm-generic-records.md,
> docs/wasm-memory-sizes.md, docs/wasm-hashmap.md and library/wasm-vec.wy.

Records can contain one non-Copy field: a Buffer or an acyclic record with the
same one-owner rule. All other fields use existing non-unit Copy storage. Generic
record parameters still require concrete Copy storage; generic functions and
source invariants/contracts remain refused. No compiler-recognized Vec algorithm
is introduced.

```text
type Vec<T>:
    data: Buffer<T>
    length: U64
    capacity: U64

fn rebuild(input: Vec<U8>) -> Vec<U8>:
    let length = input.length
    let capacity = input.capacity
    let data = move input.data
    return Vec(move data, length, capacity)
```

This bounded source implementation awaits independent review and focused runtime
qualification. It does not establish generic Vec functional contracts or enable
the historical library unchanged.

## One logical owner, ordinary physical cells

Record properties follow descriptor fields. A Buffer-containing record is linear:
it cannot be copied or implicitly discarded. Whole moves, private by-value calls,
Result payloads, branch/loop joins, lexical scopes and mutable local replacement
use existing logical availability. Every normal path must transfer its owner.
Replacement succeeds only after the complete checked RHS definitely consumes the
old wrapper. A rejected RHS never restores availability.

Physical layout still uses the common field offsets, alignment, flattened ABI and
Cell codec. Frame storage may contain an i64 Buffer token plus Copy scalar leaves;
such physical loads do not grant source Copy permission. Nested/trailing padding
retains its layout meaning. Existing field/carrier/descriptor/depth/frame limits
remain; arrays and heap Buffer elements cannot contain owner records.

The selected template pass checks owner roles and symbolic carrier lower bounds
without choosing a concrete element stand-in. It validates unused definitions,
cycles, duplicate fields, invalid types and more than one owner. Copy constraints
also apply to every type argument, including phantom arguments. A template that
contains one owner reports `structural-one-owner-template`; Copy-only templates
retain their previous validation label. Concrete specialization rechecks actual
carrier/layout bounds.

## Direct extraction consumes the wrapper

`move input.data` uses the existing move/Field AST. It is allowed only when input
is a bound owned one-owner record and data is its direct sole non-Copy field.
The explicit Extract IR consumes the WHOLE wrapper and returns that field; Copy
remainder fields are discarded. Save length/capacity first. No subsequent wrapper
or Copy-field read, write, whole move or second extraction is valid.

Nested wrappers require one extraction per binding. Deep/dynamic/borrowed
extraction, Copy-field extraction and general partial-move graphs remain refused.
Ordinary projection from any non-Copy record temporary is refused, including a
Copy field: projecting `relay(move input).length` would otherwise lose the owner.
Bind the complete returned record first, then read its Copy field or extract its
owner explicitly.

Field/whole assignments through a borrowed owner and owned field replacement
remain closed. Reconstruction uses a fresh complete record. Fallible release can
save Copy fields, extract data, match `memory.try_release`, then reconstruct the
wrapper on Err with the returned exact owner. Full buffers still require draining;
there is no automatic destructor or implicit release.

## Whole-root borrow permissions

Every field projection retains the canonical whole-record root identity and
parent availability. Shared and unique field arguments use the existing loan
ledger; distinct fields do not imply disjointness. For example, precompute
`index = vector.length` before `memory.init(&mut vector.data, index, value)`.
Reading vector.length as a later argument conflicts with the earlier unique root
loan. Shared reads remain possible when their root permission allows them.

The same rule applies through borrowed record parameters. Token loads under
borrow permission cannot become owned copies, and owner/header stealing or
reference escape remains refused. Conditional init/swap payload evaluation keeps
its skipped/evaluated ownership join even when the other owner is a record.

## Concrete identity and receipts

One semantic type formatter derives Buffer element and record-instance names from
interned concrete types, including substituted template arguments and array
lengths. `Buffer<T>` inside Vec<U8> reports Buffer<U8>, while Vec<U64> differs.
Equal-sized nominal records keep distinct identities and Buffer allocation type
IDs; byte width or flattened carrier count is never an identity key. The obsolete
AST-only formatter is removed.

Owner aggregate receipt entries add `ownership:"linear"`,
`implicit_discard:false` and the direct `owner_field` name. Existing canonical
field slots/byte offsets/sizes remain. Copy-only entries gain no ownership fields.
The typed-buffer v2 profile, six helpers, memory map, operator grants and fresh
source/module/host/proof/artifact binding are unchanged.

Generic function bodies still need a semantic type-parameter representation before
physical register allocation. No U8 placeholder proof, owner element, ZST/drop,
growth, String invariant, unsafe VC, reusable admission or performance completion
is added by this record slice.
