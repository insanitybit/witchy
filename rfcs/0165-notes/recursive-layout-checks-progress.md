# Recursive layout checks: sealed integration slice

This slice prepares nominal layout consumers for recursive values. It does not
remove the production `OpaqueNominal` catalog refusal and does not establish
recursive source coverage.

## Changes

- Source ABI correspondence traverses a finite product of layout identity and
  unqualified source type. Every newly visited pair checks its descriptor and
  enqueues all child obligations. Repeated pairs close graph edges, not sibling
  checks. A 65,536-pair limit rejects excessive expansion rather than accepting
  unfinished work.
- Memory descriptor reads resolve authenticated nominal bindings at the
  physical-layout boundary. Reclamation-layout support uses an explicit graph
  worklist, checking every physical node without recursive Rust calls.
- Backend physical layout consumers resolve nominal bindings while preserving
  nominal IDs in SSA reference types and pointer metadata. Fixed-field traversal
  propagates missing-binding errors instead of returning an empty child list.
- Nullable collection slots remain refused. Nullable fixed fields retain their
  checked presence and ownership requirements.

## Focused evidence

Commands run in the integration worktree with the existing warmed target:

```sh
export CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= taskpolicy -c utility cargo test -p witchy-wir --lib --features native source_recursive_layout_graph_checks_every_pair_and_child -- --nocapture
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= taskpolicy -c utility cargo test -p witchy-wir --lib --features native recursive_drop_layout_graph_requires_bound_supported_children -- --nocapture
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= taskpolicy -c utility cargo test -p witchy-wir --lib --features native nullable_owned -- --nocapture
```

Results: one source-correspondence test, one reclamation-layout test, and ten
nullable ownership tests passed. The newly built native WIR test binary also ran
`canonical_wasmir::drop_worklist::tests`: four passed, including the 25,000-node
chain, wide graph, shared-edge/underflow, and nullable-child cases. These counts
overlap: the nullable-child test belongs to both selections.

The source test covers a bound recursive record, an unbound identity, a
mismatched sibling scalar, incompatible source types sharing one candidate
layout, and a tagged-shape mismatch. These are structural ABI probes, not
production source-to-execution evidence.

## Remaining integration obligations

1. Derive stable instantiated nominal identities and bindings from the canonical
   checked source; update aggregate construction and layout lookup together.
2. Complete checker and lowerer consumers that still assume reference identity
   equals its physical descriptor ID. Keep the production seal until the coupled
   source, ownership, backend, and admission probes pass.
3. Run actual recursive source programs on the source interpreter, SafeWIR oracle,
   and admitted Wasm; then update coverage and deferred tests from those results.

The preceding nullable-source slice `e09a9d6a4` was rejected by the merge queue:
`admitted_aggregates_render_structurally_as_the_interpreter_prints_them` fails in
lowering with `MissingLayout(Tuple([Named("Int", []), Named("String", [])]))`
for the fixture containing `List(Option(String))`. That source-layout regression
must be repaired before the combined branch can land. The recursive checks above
do not repair or waive it.

Compiler-wide memory safety and optimizer correctness are not established.

## Concrete generic shape follow-up

Canonical aggregate shapes now substitute concrete user type arguments into
fields and key recursive occurrences by the instantiated type. Recognition
checks unused arguments too, so a phantom parameter cannot hide an unresolved
type. Both a depth limit and a 4,096-node instantiated work budget reject
unbounded expansion before materializing the next substituted field.

The source ABI matcher accepts a user generic reference only when its candidate
layout matches the independently instantiated field graph. `Box(Int)` cannot
use `Box(String)`'s payload layout or vice versa.

Focused evidence on the same absolute warmed target:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= taskpolicy -c utility cargo test -p witchy-types --lib canonical_aggregates::recursive_shape_tests -- --nocapture
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= taskpolicy -c utility cargo test -p witchy-wir --lib --features native source_generic_layout_graph_checks_instantiated_payload_types -- --nocapture
```

Five shape tests and one ABI test passed. This is not a claim that complete
generic programs lower or execute: source layout construction and constructor
lookup still require the remaining integration work above.

## Recursive catalog follow-up

Source layout projection now reserves opaque identities for cycle-participating
user heap types, then binds each to its concrete record body. A domain-separated
SHA-256 of the instantiated aggregate key determines the identity independently
of traversal order. Acyclic types continue through concrete layout construction.

Cycle membership is determined from the completed reachable user-type graph.
Review caught a root-order bug in an earlier active-path-only implementation:
`A -> [B, C], B -> A, C -> B` could leave `C` unclassified. The corrected graph
walk and a regression for that graph are included.

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= taskpolicy -c utility cargo test -p witchy-lower --lib canonical_layouts::tests -- --nocapture
```

Nine projection tests passed on the same warmed target, including canonical
binding roundtrip, mutual and branching cycles under reversed root order,
acyclic nested aggregates, nested lists, Bytes, tuples, and dictionaries.

The separate review observation about opaque identities embedded in inline
field descriptors was withdrawn as a reachable defect: catalog construction
and decoding reject such inline descriptors before checker entry. This does
not relax that catalog invariant or establish general checker soundness.

Remaining: source value/constructor lookup must consistently retain nominal
reference identities while using the bound physical body for places and sizes.
The initial production opaque-layout refusal remains present.

## Checked-source lowering and static-data follow-up

A checked program that constructs `Branch(1, Leaf, Leaf)` through a reachable
`build` function and matches the result now lowers to nominal SafeWIR references.
The fixture selects the checked declaration's qualified name rather than
assuming a bare callable name. Its next step deliberately observes the existing
initial opaque-layout refusal; this is not execution evidence.

The focused selector
`recursive_source_type_lowers_to_the_catalog_nominal_before_checker_seal`
passed in the `witchy-lower` library target.

Readiness review found static-data judgments still inspecting the zero-sized
opaque descriptor. They now resolve the physical body, with the passing WIR
selector `nominal_static_data_checks_resolve_physical_storage`. Details and the
limited security claim are recorded in
`security-eval/findings/BUG-canonical-nominal-static-data-physical-extent.md`.

No new ownership authority was granted to WriteBack parameters or alias-result
contracts. Their separate unsupported cases remain fail-closed.
