# Canonical library preparation confused templates and concrete payloads

## Observed failure

The regex landing gate failed the unchanged
`example_tests::example_sweeps::all_std_modules_type_check` test:

```text
std/chan.witchy: canonical message bridge remains without a concrete checked payload
```

After that repair the same focused test exposed:

```text
std/dynamic.witchy: closed Dynamic construction has no checked concrete payloads
```

The gate log is
`state/merge-queue/logs/20260910-101147-impl~rfc0165-regex-spans-38402-78.log`.
Four additional counted failures were cancelled tests, not independent defects.

## Cause and correction

Canonical entry scope includes generic library templates, but the existing
callable runtime inventory excludes functions with free signature variables.
Message preparation used that exclusion when collecting payloads but not when
retaining bodies, so an unexecutable template survived into its closing pass.
Census and retention now use the same concrete-runtime predicate as the callable
witness. Every retained body is still converted and fallibly rechecked; an
unstamped concrete bridge still rejects.

`dynamic.field` reconstructs an envelope around an existing existential payload.
That is not a fresh concrete payload pack. Dynamic preparation now distinguishes
the two using the current type table's existential-pack facts. Complete runtime
plan authentication still precedes the descriptor-only path. Open typed source
does not receive a fabricated payload sum or executable payload authority.

A proposed blanket skip for modules without `main` was rejected and never
applied. Neither repair adds raw fallback or relaxes admission checks.

## Focused evidence

Use the absolute warmed target
`/private/tmp/witchy-rfc0165-reflect-specialization-target`.

- `cargo test -p witchy-types --lib canonical_messages::tests::`: five pass,
  including loader-ownership refusal and distinct nominal payload tags.
- `cargo test -p witchy --features witchy-wir/native --bin witchy
  example_tests::example_sweeps::all_std_modules_type_check -- --exact`: passes.
- `cargo test -p witchy --features witchy-wir/native --test misc
  rfc0165_oracle::dynamic_`: the real Dynamic program agrees across source,
  independent oracle, and admitted Wasm, with zero leaks.
- The strengthened `dynamic_library_forwarding_does_not_mint_closed_payload_authority`
  test calls `dynamic.field`, checks that no closed payload sum was minted, and
  requires production artifact compilation to remain Unsupported; passes.
- Both indexed-list update tests and CLI hello/wrap/jq/plugin_host/patterns
  pass on the repaired integration.

Bounded independent review found no blocking safety issue; its request to make
the negative fixture exercise actual forwarding was incorporated and tested.
This evidence does not establish compiler-wide memory safety.
