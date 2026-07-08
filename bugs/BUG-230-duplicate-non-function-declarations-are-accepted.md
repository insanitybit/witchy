# BUG-230: Duplicate non-function declarations are rejected

Status: FIXED
Severity: MED
Type: SOURCE
Verified: 2026-07-08 FIXED on `worktree-wt-bug-230-close`
Component: language frontend, declaration uniqueness, type/method namespaces

## Summary

Duplicate declaration handling is now source-level and deterministic for the
non-function namespaces that were still open after the first BUG-230 narrowing.

The pre-lowering declaration uniqueness pass rejects:

- duplicate top-level constants;
- duplicate type aliases;
- type/type-alias name conflicts;
- duplicate record fields;
- duplicate sealed capability fields;
- duplicate type names, constructors, inherent methods, trait methods, and trait
  impl heads covered by the earlier BUG-230 work.

This closes the remaining release-facing inconsistency where constants, aliases,
and record/capability fields could overwrite or ambiguously reshape a program
instead of producing a direct declaration diagnostic.

## Evidence

- `crates/witchy-types/src/typeck.rs::check_unique_declarations` checks the
  constant, alias, type, constructor, field, method, trait-method, and trait-impl
  namespaces before lowering.
- `crates/witchy-types/src/typeck_tests.rs::duplicate_declarations_are_rejected`
  covers the formerly remaining cases: duplicate constants, duplicate aliases,
  alias/type conflicts, duplicate record fields, and duplicate capability fields.

Validation on 2026-07-08:

```console
CARGO_TARGET_DIR=target-codex cargo test -p witchy-types duplicate_declarations_are_rejected -- --nocapture
```

Result: passed, 1 test.

## Residuals

This closure does not cover adjacent hygiene bugs in different namespaces:

- BUG-441 tracks collisions between handwritten mangled function names and
  lowered method names.
- BUG-443 tracks source-spellable compiler-generated value names.
- BUG-449 tracks the deliberate prelude redefinition exception for bundled
  examples.
