# BUG-186: Regex invalid-pattern contract is split between docs and runtime behavior

Severity: MED
Status: FIXED
Verified: 2026-07-08 fixed on master 64bf3332
Fixed: 2026-07-08
Component: `std/regex`, native runtime, generated stdlib docs

## Current status

This row is stale. Current source implements the loud-error contract:

- `crates/witchy-runtime/src/native.rs` compiles the pattern with
  `regex::Regex::new(pattern)` and maps parser failures to
  `invalid regex pattern ...`.
- `std/regex.witchy` and `spec/stdlib.md` still document invalid patterns as
  loud errors.
- `example_tests::regex_invalid_pattern_is_loud_on_both_backends` covers the
  public API on the interpreter and compiled backend.

Focused verification on 2026-07-08:

```text
$ CARGO_TARGET_DIR=target-codex-docs cargo test regex_invalid_pattern_is_loud_on_both_backends -- --nocapture
test example_tests::regex_invalid_pattern_is_loud_on_both_backends ... ok
```

## Historical problem

`std/regex` and the generated stdlib reference promise that an invalid regular
expression pattern is "a loud error, not a silent non-match." The runtime does
the opposite: invalid patterns are caught in the native `match_spans` helper and
returned as the same empty string used for "no match."

The behavior is consistent across interpreter and compiled WASM, but it
contradicts the public API contract and makes invalid user-supplied patterns
indistinguishable from valid patterns that simply match nothing.

## Repro

`scratch/repro-regex-invalid-pattern-silent.witchy`:

```witchy
import regex

fn main(console: Console):
    print(console, __render(regex.matches("(", "abc")))
    print(console, __render(regex.find("(", "abc")))
    print(console, __render(regex.find_all("(", "abc")))
```

The source checks:

```console
$ ./target/debug/witchy check scratch/repro-regex-invalid-pattern-silent.witchy
scratch/repro-regex-invalid-pattern-silent.witchy: ok
```

Source run returns ordinary non-match values:

```console
$ ./target/debug/witchy scratch/repro-regex-invalid-pattern-silent.witchy
false
None
[]
$ echo $?
0
```

The sandboxed compiled path behaves the same:

```console
$ ./target/debug/witchy sandbox scratch/repro-regex-invalid-pattern-silent.witchy
sandboxing `scratch/repro-regex-invalid-pattern-silent.witchy` — granted exactly: Console
false
None
[]
$ echo $?
0
```

Parity therefore passes, but for the silent behavior:

```console
$ ./target/debug/witchy parity scratch/repro-regex-invalid-pattern-silent.witchy
✓ scratch/repro-regex-invalid-pattern-silent.witchy: interpreter and compiled WASM agree (3 line(s) of output)
parity-stats outcome=agree compared=3 file=scratch/repro-regex-invalid-pattern-silent.witchy
```

## Historical code evidence

- `std/regex.witchy:1-4` documents invalid patterns as a loud error, not a
  silent non-match.
- `spec/stdlib.md` repeats the same generated documentation for `regex`.
- `crates/witchy-runtime/src/native.rs` has contradictory comments: the module
  header says invalid patterns are loud errors, but `regexp::match_spans`
  explicitly catches `regex::Regex::new(pattern)` failures and returns
  `Ok(Value::Str(String::new()))`.
- The empty string is also the public encoding for "no matches", so callers
  cannot distinguish invalid syntax from a valid regex with zero matches.

## Why this matters for 0.1

Regex is a basic stdlib tool and likely to receive user-supplied patterns in CLI
and text-processing programs. A public reference that promises validation while
the runtime silently treats bad patterns as no-op/non-match makes the API feel
unfinished and can hide configuration mistakes.

The release decision can go either way: total regex functions that never abort
may be a reasonable design. The bug is that the public contract, runtime
comments, and behavior currently disagree.

## Expected behavior

Pick one coherent contract:

- If invalid patterns should be loud errors, make `match_spans` surface the
  regex parser error on both backends and add parity tests for
  `matches`/`find`/`find_all`.
- If regex matching should be total, update `std/regex.witchy`,
  `spec/stdlib.md`, and runtime comments to say invalid patterns return the
  same values as no match, or add a separate validation/compile API so callers
  can tell invalid syntax from no match.

## Acceptance criteria

- The repro exits non-zero with a clear invalid-regex diagnostic.
- Interpreter and compiled behavior remain parity-clean.
- Regression coverage exists for invalid patterns through the public
  `std/regex` API.
