# BUG: checker-accepted scalar `match` CFG is refused by the canonical encoder

Status: fixed on `impl/rfc0165-interpreter` (2026-09-05): the lowering owned it

## Finding

Three foundation-subset programs lower to canonical SafeWIR, pass initial and
final validation, and are then refused by the private WasmIR backend:

```text
examples/hello/src/hello.witchy    Admission(Lowering("Lowering(UnsupportedControlFlow { function: FunctionId(1), operation: \"unstructured or unreachable block\" })"))
examples/compute/src/compute.witchy Admission(Lowering("Lowering(UnsupportedControlFlow { function: FunctionId(2), operation: \"unstructured or unreachable block\" })"))
examples/signs/src/signs.witchy    Admission(Lowering("Lowering(UnsupportedControlFlow { function: FunctionId(0), operation: \"unstructured or unreachable block\" })"))
```

Each named function is a scalar `match` with a wildcard arm (`classify`,
`fact`, `step`). Scalar `match` lowering is inside the foundation subset, so
this is a SafeWIR-to-WasmIR mapping gap (B7), not language coverage: the
checker's CFG theory accepts a block shape the structured-control encoder has
no rule for. Until the mapping is total over checker-accepted CFGs, B7 cannot
claim a reviewed exhaustive mapping.

## Current containment

The refusal is fail-closed and, since this change, carries the encoder's own
diagnostic (`CanonicalAdmissionError::Lowering(String)`) instead of a bare tag,
so the gap is attributable from `witchy check` output.

## Required fix

Either the CFG legalization that produces the `match` lowering must emit the
structured shape the encoder's rule set covers, or the encoder must gain a
reviewed rule for the accepted shape. Whichever side changes, the executable
SafeWIR interpreter and the emitted Wasm must agree on the three programs, and
the encoder's acceptance must remain a function of the validated CFG, never of
a guessed source pattern.

## Evidence

```sh
/private/tmp/witchy-rfc0165-interpreter-target/debug/witchy check examples/signs/src/signs.witchy
```

## Fix (2026-09-05)

`lower_match` allocated a fallthrough block for every arm before knowing
whether the pattern was refutable. For a wildcard arm the dispatch block
branches unconditionally into the arm, so that fallthrough had no predecessor:
a legitimate SafeWIR block by validation but unreachable from entry, which the
structured backend correctly refuses rather than guessing it is dead. The
lowering now creates a fallthrough only for an arm that can fail (a refutable
pattern or a guard) and traps only when a refutable last arm exists.
`examples/signs` runs end to end; `hello` and `compute` now stop at string
interpolation, the next in-subset gap. A second encoder-side limit remains
open: `if c then A else trap`, where one side of a `BranchIf` only terminates,
finds no join (`find_join` requires a block reachable from both sides).
