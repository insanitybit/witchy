# BUG: canonical entry scope dropped a bare `main`, admitting a module without the program

Status: fixed on `impl/rfc0165-interpreter` (2026-09-05)

## Finding

The linker leaves the entry module's `main` unqualified and qualifies its
other functions with the entry module name (`app.helper`). Four canonical
predicates decided "is this function part of the checked program" with

```text
name == entry || name.starts_with("{entry}.")
```

which only includes `main` when the entry module is itself named `main`. For
`examples/hello/src/hello.witchy` and every other file not named
`main.witchy`, `main` was silently excluded from the callable ABI manifest,
the proper-tail ordinal map, the layout derivation, and canonical lowering.
The remaining functions lowered, validated, and were admitted:

```text
probe_scope.witchy (fn answer + fn main)  -> admitted module: functions=1 exports=memory,f0
main.witchy        (same source)          -> admitted module: functions=2 exports=memory,f0,f1
```

`witchy check` reported `ok` for the first, and the census counted it as
passing, although the admitted artifact did not contain the program. This is
the failure mode the ledger warns about: coverage evidence that is not the
program. No unsafe code was emitted, but "admitted" did not mean "the checked
program", and the CLI could not run any canonical program because the entry
was never present (`failed to find function export`).

Every pipeline test happened to use a module named `main`, so the suite could
not observe the defect. `async_migration.rs` already carried the correct rule
(`function == "main" || function.starts_with("{entry}.")`).

## Fix

One shared predicate, `witchy_types::pipeline::is_canonical_entry_function`,
replaces the four copies (`pipeline.rs` binder, `typeck::for_canonical_functions`,
`canonical_lower::is_canonical_source_function`, `canonical_layouts`). The
admitted artifact's launch entry is derived from the ABI of `main`, not of the
entry module name. A pipeline test links a module named `app` and asserts both
`main` and `app.helper` are bound and the artifact records a launch entry.

## Related

The launch protocol itself was also missing: canonical modules export `f<id>`
and no parameterless `run`, so the runtime now carries a validated entry
descriptor in `AdmittedCanonicalArtifact` and mints capability tokens at
launch (`Vm::call_canonical_entry`). And the backend emits `return_call` for
proper tail sites while the production engine set `wasm_tail_call(false)`, so
`main.witchy` failed at Wasmtime compile (`failed to compile:
wasm[0]::function[1]`); see `BUG-canonical-tail-call-engine-disabled.md`.
