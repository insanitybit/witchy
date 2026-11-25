# Generic copyable records in direct Wasm

Source record templates can be specialized with the existing non-unit copyable
scalar, record and fixed-array types. Constructors use a concrete expected type:

```text
type Box<T>:
    value: T

type Pair<A, B>:
    first: A
    second: B

fn boxed(x: U8) -> Box<U8>:
    return Box(x)

fn main(console: Console<Write>):
    let p: Pair<Box<U8>, [U16; 2]> = Pair(boxed(3), [10, 20])
    console.print_int(U8.to_u64(p.first.value))
```

The same descriptor supplies field types, scalar slots, aligned layout metadata,
and direct-call argument/result shapes. Records execute as Wasm locals and
multi-value calls. No additional runtime representation, allocation or memory
access is introduced. The source names `Box` and `Pair` have no special meaning.

A local annotation, function parameter/result, or an enclosing aggregate field
can supply constructor context. Without one, the field values settle the type
when their own types are known: `Pair(first, flag)` with `first: U64` and
`flag: Bool` is a `Pair<U64, Bool>`, and `Pack(move data, 0)` with
`data: Buffer<U64>` is a `Pack<U64>` (2026-11-15,
`tests/fixtures/generic/constructed.wy`). A bare literal settles nothing, so
`let x = Box(1)` is still refused and asks for the type to be written.
Existing bound values keep their concrete widths. `Box<U8>` and `Box<U16>` are
distinct types even when a particular value fits both. Different record names
remain nominally distinct. Repeated identical applications share one concrete
layout; argument identity includes scalar width and nested aggregate identity.
The specialization sequence and resulting descriptor IDs are deterministic for
the same source, not stable across unrelated source edits.

## Template validation and specialization

Every selected template is structurally validated, including unused templates.
Validation checks duplicate/reserved names, distinct parameters, duplicate fields,
reference existence and arity, supported field forms and recursive dependencies.
Parameter names cannot collide with declared record names or the supported
reserved scalar/Console/main names. Substitution uses a separate local parameter
environment, never textual replacement. Borrowed, owning, slice, unit-field,
invariant and unknown types fail closed. Recursive template dependencies are
unsupported, including conservative refusals where a phantom parameter could
otherwise make a cycle finite. Finite nested applications such as
`Box<Box<U8>>` are supported.

Template validation uses symbolic minimum slot counts: a non-unit copyable
parameter occupies at least one scalar slot. It does not manufacture layouts
using arbitrary parameter choices and does not assert that every possible
specialization satisfies the limits. Concrete requested specializations must
also pass the complete existing flat-shape and storage-layout checks. An unused
`Pair<T>` can be valid while a later `Pair<[U8; 17]>` exceeds the 32-slot bound.

Template limits are eight parameters, 1..32 fields, nesting depth 16 and 4096
type visits per selected template. Concrete source-type nesting is bounded by
16, and the existing active-record nesting bound remains 16. An array on its own
holds 1..256 elements; a record or sum, and a function's parameters and result, hold at most
32 numbers.
At most 128 concrete aggregate descriptors exist across records and arrays;
this also bounds record specialization count. Limits fail closed before emission.

`selected_types` continues to list all selected record names. The coordinator's
additive `generic_record_templates` receipt field records each generic template's
name, parameters and `structural-copyable-template` validation status. Concrete
specializations alone appear in `aggregate_layouts`, with record identities
containing every concrete argument. Unused generic templates create no aggregate
layout. The additive field is omitted when there are no generic templates, so
existing nongeneric receipts retain their format. Template coverage is not a
proof admission or a proof that all possible specializations are executable.

## Remaining work

Generic functions, owning storage, regions/loans, unsafe primitives, allocation,
Vec/String and reusable generic proof admission remain unsupported. Runtime
contracts and unsupported proof forms still fail closed through their existing
checks. This change neither weakens those checks nor establishes the full
Witchy-to-Wasm ownership or proof milestones. The separate scalar lemma slice
retains its own exact supported scope.
