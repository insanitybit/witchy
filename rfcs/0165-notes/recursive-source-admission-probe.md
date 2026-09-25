# Recursive source admission probe

Integration base: `935ee4469`, `impl/rfc0165-recursive-layout-checks`.

`tests/rfc0165_recursive.rs` is a standalone integration target so unrelated
unfinished `misc` fixtures do not prevent this probe from compiling.

## Current focused evidence

After nominal descriptor hardening and removal of the blanket recursive seal,
the complete source/oracle/admitted-Wasm probe passed in 0.26 seconds. All
three executions returned `5`; the oracle and Wasm heap audits reported zero
leaks despite storing the same child in both branch slots.

The `structural_contracts::recursive_layout_transport` module also passed. It now submits an actual
candidate with pointer-bearing nominal static data, empty bytes, and a zero
extent, and requires `PointerBearingStaticData`. Unbound, rebound, inline,
and malformed transport rejection coverage is retained. A well-bound unused
nominal catalog may validate, but that grants no unchecked operation authority.

The two focused memory-checker probes passed: physical descriptor resolution
through a nominal identity, and rejection of direct `Reclaim` for a legal
pointer-bearing list. The initial proposed nominal-inline-list fixture was
not integrated: catalog rules correctly make that representation inadmissible.

This is focused recursive coverage, not a fresh corpus or merged result.

## Prior refusal, now resolved

The checked source constructs `Branch(1, child, child)` with `child = Leaf(2)`.
The source interpreter passes the independently specified output `5`.
Before enablement, production compilation failed during initial validation:

```text
InitialValidation(Safety(InvalidLayoutBundle("recursive nominal values require canonical ownership and destruction support")))
```

That earlier run compiled successfully and failed at the boundary in 0.24
seconds. The current evidence above supersedes that refusal.

Reproduce from the integration worktree:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target \
  taskpolicy -c utility cargo test --test rfc0165_recursive -- --nocapture
```

A bounded independent review examined physical-layout consumers, nullable
slot initialization and destruction, and rejection obligations masked by the
seal. It found one remaining raw descriptor lookup in packed-element reclaim
classification; that lookup now uses physical descriptor resolution. No
definite current bypass was established by the review. All operation-level
validation remains mandatory.

The unrelated hex fixture annotation and `List(Option(String))` regression
remain unresolved. No full gate or fresh corpus run accompanies this probe.
