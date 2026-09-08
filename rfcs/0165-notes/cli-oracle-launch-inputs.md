# CLI parity oracle launch inputs

The CLI parity oracle must use the entry interpreter, not the generic function
interpreter. An admitted `main` with a `List(String)` parameter needs an owned
argument list even when there are no command-line arguments.

`safewir_oracle_agrees` now accepts an explicit launch-argument slice and calls
`interpret_entry_program`. The current CLI parity caller supplies an empty
slice, matching its existing source-interpreter and Wasm invocation. This does
not add nonempty argument parsing to the parity command or change entry identity.

Focused evidence: `parity_oracle_materializes_empty_and_nonempty_launch_arguments`
passed, one test, 0.16 seconds. It compiles an actual checked argument-entry
program, checks empty and nonempty oracle inputs, preserves the helper's leak
audit, and requires an omitted-input mismatch to fail. Its expected output is
supplied independently; this is not a new end-to-end Wasm parity result.

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target \
  taskpolicy -c utility cargo test -p witchy --bin witchy \
  parity_oracle_materializes_empty_and_nonempty_launch_arguments -- --nocapture
```

No fresh corpus or full gate was run for this slice. The remaining recursive
admission and nullable-collection work is separate.
