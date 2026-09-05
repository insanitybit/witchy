# Detached Wasm cache bypassed canonical admission

Before `c615ab144`, checked-source and embedded compilation could return a
user-writable `.wasm` cache entry without reconstructing or verifying canonical
admission authority. A cache file could therefore be modified after its original
compilation and later emitted or launched without `ValidatedSafeWir` or
`AdmittedCanonicalArtifact` participating in that invocation.

The foundation disables these byte caches. Checked modules now cross canonical
validation and exact-byte admission on every compilation. Ordinary compiled run
also carries `AdmittedCanonicalArtifact` directly into `Runtime::spawn_admitted`.
Caching may return only after a persisted format authenticates an admission
receipt and verifies the exact bytes before use.

Focused evidence:

- `canonical_pipeline::tests::admitted_artifact_carries_checked_launch_contract_and_refuses_mutation`
- `canonical_source_witness_tests::browser_compile_preserves_the_single_admitted_launch_section`
- `cargo check --lib` with the production callers routed through the new boundary

This finding does not claim all artifact producers are audited or that the
compiler-wide memory-safety goal is complete.
