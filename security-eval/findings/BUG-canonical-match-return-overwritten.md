# Canonical match lowering overwrites early-return terminators

Status: confirmed lowering defect; repair in progress.
Observed revision: `cd341fcd8` (also present at `e6045165f`).

## Reproducer and evidence

Run the existing focused integration test with the warmed target:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target \
  WITCHY_CANONICAL_PLAN_DEBUG=1 \
  taskpolicy -c utility cargo test --test misc \
  typed_errors::duration_typed_errors_are_matchable_and_bridge_to_string \
  -- --exact --nocapture
```

The existing executable built at `e6045165f` reported:

```text
canonical owned edge mismatch at BlockId(4): first {ValueId(0)}, now {} (from BlockId(84))
OwnedReleasePlacement("IncompatibleOwnedEdge(BlockId(4))")
```

The relevant source is `duration.parse`: inside a loop retaining its input
String, a match arm returns an error containing that String, while another arm
continues the loop. A returning arm must not flow into the match join.

## Traced cause

`lower_match` in `crates/witchy-lower/src/canonical_lower.rs` records terminal
arm blocks among ordinary arm ends, then replaces their terminators with
branches to the common join. A moved input reference consequently reaches the
loop backedge without its owned token. `canonical_owned` correctly rejects the
incompatible incoming sets.

This is a source control-flow defect, not evidence that the ownership rule is
too strict. The observed program fails closed; this finding does not establish
an admitted memory-safety exploit or an executed scalar miscompilation.

## Required repair and acceptance

- Preserve terminal arm terminators and omit their join edges.
- Plan binding and result lanes using only continuing arms.
- Preserve proper-tail obligations and avoid fabricated result values.
- Keep independent ownership and final-validation rules unchanged.
- Exercise early-return and continuing paths through source execution,
  independent expected results, SafeWIR interpretation, admitted Wasm, and
  reference-release checks; rerun the duration reproducer after integration.
