# Carried capability passes admission but fails runtime capture typing

Status: reproduced; implementation cause not yet established.

Integration source: `1f870b2c5` on `impl/rfc0165-task-capture-integration`.
The existing example is unchanged. No network I/O is required by its body.

## Reproducer

From the integration worktree:

```sh
/private/tmp/witchy-rfc0165-reflect-specialization-target/debug/witchy check examples/carried_state/src/carried_state.witchy
/private/tmp/witchy-rfc0165-reflect-specialization-target/debug/witchy parity examples/carried_state/src/carried_state.witchy
```

The check command succeeds. The parity command exits 3:

```text
interpreter: Ok(["ok: counted rows in users", "denied: secrets is outside this handle (scoped to users)"])
compiled: Err(closure capture has wrong capability kind)
```

The runtime rejection originates in `runtime/host/closure_owner.rs` in
`witchy-runtime`. The example constructs and pattern-matches a sealed `Postgres`
capability record containing a `Net[Connect, Tcp]` and a string policy field.
Whether the mismatch originates in wrapper lowering, admitted type identity,
or runtime token classification remains to be determined.

## Required correction

Preserve the distinction between an owned capability-bearing aggregate and its
host-minted capability field. Do not relax the host capability-kind check or
authorize arbitrary externrefs. Require the unchanged example to agree on the
source, admitted-Wasm, and independent-oracle legs, with ownership cleanup.
Successful admission alone is not executable coverage or soundness evidence.

## Correction and focused evidence

Integration commit `bf8d399a3` adds the missing existing Net kind to runtime
capture authentication, requiring a host `NetAuthority`. `Postgres` remains
an owned aggregate, never a host capability token. Wrong kinds, forged integer
externrefs, and substituting the wrapper for its field are rejected.
`carried_state_unchanged_expected_three_legs` passes with an explicit reject-all
Net grant, expected output, and zero oracle/Wasm leaks. The combined integration
run recorded 19 passing tests in `/tmp/witchy-task-net-integrated-focus.log`.
Landing remains pending.
