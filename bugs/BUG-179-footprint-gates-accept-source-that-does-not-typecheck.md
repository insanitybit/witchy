# BUG-179: Footprint gates accept source that does not typecheck

Severity: MED
Status: FIXED
Verified: 2026-07-08 fixed on master 33afd230
Fixed: 2026-07-08
Component: `witchy caps`, `caps-diff`, `grants-check`, capability footprint gates

## Problem

The footprint commands present themselves as release and supply-chain gates over
the program's source authority. They parse source and compute signatures, but do
not link or type-check before reporting a successful footprint result.

As a result, `witchy caps` can exit 0 and print an authoritative-looking
footprint for a file that `witchy check` rejects. `caps-diff` can then say an
invalid update is "OK: no widening", and `grants-check` can say an empty grant
document matches invalid code exactly.

This is distinct from BUG-178. BUG-178 is about generated public APIs disappearing
because the footprint path does not expand `comptime:` / derive-generated code.
This bug is the broader validation split: even ordinary type errors are ignored
by the footprint gates.

## Repro

`scratch/repro-caps-accepts-type-invalid.witchy`:

```witchy
fn main(console: Console):
    missing(console)
```

`check` rejects the source:

```console
$ ./target/debug/witchy check scratch/repro-caps-accepts-type-invalid.witchy
type error: `main`, line 2: call to unknown function `missing`
$ echo $?
1
```

`caps` accepts the same source and reports a footprint:

```console
$ ./target/debug/witchy caps scratch/repro-caps-accepts-type-invalid.witchy
Host-capability footprint of scratch/repro-caps-accepts-type-invalid.witchy:
  main   Console
  total  Console
$ echo $?
0
```

`caps-diff` can also approve a non-checking update:

```console
$ cat /tmp/witchy-caps-valid-old.witchy
fn main(console: Console):
    print(console, "ok")

$ ./target/debug/witchy caps-diff /tmp/witchy-caps-valid-old.witchy scratch/repro-caps-accepts-type-invalid.witchy
Capability footprint diff /tmp/witchy-caps-valid-old.witchy -> scratch/repro-caps-accepts-type-invalid.witchy:
  old total:  Console
  new total:  Console
  added:      (none)
  removed:    (none)
OK: no widening — the newer version demands no new authority on either axis.
$ echo $?
0
```

`grants-check` accepts an empty grant document for the invalid Console-only
program because `Console` is outside grant-document checking:

```console
$ ./target/debug/witchy grants-check scratch/repro-caps-accepts-type-invalid.witchy scratch/repro-comptime-footprint-empty-grants.toml
Grant cross-check: `scratch/repro-comptime-footprint-empty-grants.toml` vs the footprint of `scratch/repro-caps-accepts-type-invalid.witchy`
  code needs:  Console
  grant gives: (none)
  OK: the grant matches what the code exercises exactly.
$ echo $?
0
```

The same issue appears for grant-modeled capabilities too. The file
`scratch/repro-caps-accepts-type-invalid-net.witchy` does not typecheck, but
`caps` still exits 0 and reports `Net`:

```console
$ ./target/debug/witchy check scratch/repro-caps-accepts-type-invalid-net.witchy
type error: `main`, line 2: call to unknown function `missing`

$ ./target/debug/witchy caps scratch/repro-caps-accepts-type-invalid-net.witchy
Host-capability footprint of scratch/repro-caps-accepts-type-invalid-net.witchy:
  main   Net
  total  Net
```

## Resolution

The source-file CLI gates had already been repaired: `caps`, `caps-diff`, and
`grants-check` link and type-check before reporting footprint status. The
remaining source-string API now does the same:

- `compiler.footprint(source)` resolves bundled std imports, links, type-checks,
  and only then computes a footprint for source strings whose imports are all
  available in the bundled std library.
- `compiler.diff(old, new)` type-checks both source strings before reporting
  widening when those source strings are self-contained or std-only.
- Source strings with non-std imports keep the historical source-only behavior:
  the native API has no caller-provided module map, while the source-file CLI
  gates own strict multi-module validation. Misspelled std imports still fail
  loudly when there is a close bundled-std suggestion.
- `comptime:` source strings still fail closed because this native API cannot
  run source-file expansion safely; callers that need expanded introspection use
  the source-file CLI path.

Regression coverage:

- `tests::caps_requires_a_typechecking_source`
- `example_tests::native_compiler_intrinsics_reject_comptime_source_strings`
- `example_tests::compiler_footprint_rejects_type_invalid_sources_on_both_backends`

Focused verification on 2026-07-08:

```text
$ CARGO_TARGET_DIR=target-codex-docs cargo test native_compiler_intrinsics_reject_comptime_source_strings -- --nocapture
test example_tests::native_compiler_intrinsics_reject_comptime_source_strings ... ok

$ CARGO_TARGET_DIR=target-codex-docs cargo test compiler_footprint_rejects_type_invalid_sources_on_both_backends -- --nocapture
test example_tests::compiler_footprint_rejects_type_invalid_sources_on_both_backends ... ok

$ CARGO_TARGET_DIR=target-codex-docs cargo check -p witchy-runtime
Finished `dev` profile ...
```

## Code evidence

- `src/main.rs::analyze_file` now links and type-checks before analyzing.
- `crates/witchy-runtime/src/native.rs::compiler::footprint` and
  `compiler::diff` now use the checked source-string path before analyzing
  self-contained or std-only source strings.

## Why this matters for 0.1

The release story treats the footprint as a fact about a runnable, reviewable
program. If `caps`, `caps-diff`, and `grants-check` can return green for source
that the compiler rejects, they feel like side-channel linters rather than
authoritative gates.

This also makes generated-code failures harder to see. A missing custom derive
generator, for example, is a compile-time error in `check` but invisible to
`caps`, because the footprint path never reaches derive/comptime expansion.

## Acceptance criteria

- [x] `witchy caps scratch/repro-caps-accepts-type-invalid.witchy` exits non-zero
  with the same underlying type error as `check`, or with a clear "cannot compute
  footprint for source that does not typecheck" diagnostic.
- [x] `witchy caps-diff old invalid` exits non-zero for invalid old or new source
  before reporting widening status.
- [x] `witchy grants-check invalid grants.toml` exits non-zero before reporting grant
  sufficiency.
- [x] `compiler.footprint` / `compiler.diff` use the checked linked path for
  self-contained or std-only source strings, and continue to reject comptime
  source strings that require source-file expansion.
- [x] Regression tests cover a parse-valid/type-invalid program and a
  derive/comptime-expansion failure.
