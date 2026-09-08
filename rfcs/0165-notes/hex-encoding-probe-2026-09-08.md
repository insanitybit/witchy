# RFC-0165 hex encoding probe evidence

This is a separate post-review evidence note. It does not amend or restate the
independent review in `hex-reachability-review-2026-09-08.md`, whose scope and
no-finding verdict remain the combined diff through `e91962243`.

The correction commit `a149ea1e2` strengthens the real-standard-library
positive test in `tests/misc/rfc0165_encoding.rs`:

- It runs `witchy::interpreter::run_module_args` on the real checked module's
  linked module (`checked.module().clone()`), with no synthetic std fixture.
- The same source prints `encoding.hex_encode` for the empty string, `Hi`, and
  the multi-byte UTF-8 string `é🙂`.
- The source interpreter output is independently expected to be `""`,
  `"4869"`, and `"c3a9f09f9982"`.
- The canonical SafeWIR interpreter and admitted Wasm are each compared with
  that source-interpreter output; the Wasm run also reports zero canonical heap
  leaks.
- The focused negative still proves real `Result.map_ok` reachability and pins
  the current decode-helper refusal exactly as
  `unsupported lowering: canonical SafeWIR lowering failed: MissingBinding("encoding.hex_decode_bytes_raw")`.
  That pin is intentionally scoped to the unsupported decode helper and should
  migrate with intentional decode support.

Focused command:

```text
env CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-encoding-next-target CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= taskpolicy -c utility cargo test -p witchy --test misc rfc0165_encoding -- --nocapture
```

Result: `2 passed, 0 failed`.
