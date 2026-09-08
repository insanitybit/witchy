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
