# Canonical registry-host oracle filesystem parity

Status: corrections applied, bounded source review clear, and focused regression
execution passed.

These are two statically identified oracle/runtime parity defects, not an
executed security bypass. Production runtime behavior is unchanged.

## Counterexamples

1. **CurDir mkdir rejected by the oracle.** With a writable, unrestricted Dir,
   `root.make_dir(".")` is an idempotent runtime success. Paths such as
   `"./child"` and `"child/./grand"` also admit ordinary CurDir normalization.
   The new oracle instead rejected every `.` component through both its
   generic `host_path` and directory-creation helper. Its initial regression
   incorrectly pinned that refusal.
2. **File write over a created directory accepted by the oracle.** The sequence
   below must fail on the write. Runtime directory creation succeeds, then its
   confined file writer refuses the directory target. The oracle instead
   inserted a file at the same path as its newly recorded empty directory.

```witchy
fn main(root: Dir):
    root.make_dir("empty")
    root.write("empty", "payload")
```

## Corrections

- `crates/witchy-wir/src/canonical_interpreter/dir_create.rs` now normalizes
  CurDir and repeated separators specifically for mkdir. The registry host
  handler normalizes before joining the authenticated Dir scope. Absolute
  paths, parent traversal and NUL remain rejected; existing read-path and
  symlink restrictions are not widened.
- `crates/witchy-wir/src/canonical_interpreter.rs` now refuses `dir_write_at`
  targeting the granted root or an explicit/inferred directory before changing
  fixture files. Ordinary file writes remain supported.

## Regression assertions and evidence

Tests in `canonical_interpreter::registry_host::tests`:

- `registry_oracle_mkdir_curdir_normalization_preserves_scope`: exact normalized
  directory sets at root and subtree scope, idempotent self-directory creation,
  and retained path refusals.
- `registry_oracle_mkdir_file_conflicts_and_path_denials_are_atomic`: file
  conflicts remain refusals even through CurDir spellings, without fixture
  mutation.
- `registry_oracle_dir_write_refuses_explicit_directories_and_granted_root`:
  directory/root collisions refuse without mutation; an ordinary file write
  still succeeds.
- `registry_oracle_mkdir_fallback_never_writes_or_follows_links`: retained
  read-only fallback and symlink refusals include CurDir spellings.

The counterexamples and corrective delta were independently reviewed read-only.
No reviewer build or execution was performed. The parent confirmed the named
regressions in the 55-test green run `e4bc1ec0-ee2c-4dfd-9732-1e1e00afe43e`;
log `/private/tmp/witchy-registry-hosts-final-focused.log`. This is focused
parity evidence, not a compiler-wide safety claim.
