# BUG-094: PM build/run do not enforce lock hashes before linking

- **Severity:** HIGH
- **Status:** FIXED
- **Verified:** 2026-07-12 TESTED on `fix/rfc0054-final-trust-boundaries`
- **Component:** package manager, build/run, lock verification, registry vendor integrity
- **Found:** 2026-07-05

## Summary

The package docs say `witchy build` verifies hashes and then links/type-checks
from the lock. Current PM `build` and `run` do not verify dependency hashes,
vendored registry signatures, or vendored source hashes before compiling. They
collect `--dep name=path` flags from `witchy.toml` / `witchy.lock` and hand those
paths directly to the compiler.

That means a tampered vendored registry source can be built or run unless the
user separately invokes `verify-rune`, `verify-vendor`, or a stronger fixed
`verify` path.

## Evidence

- `book/src/packages-cli.md:84` documents `witchy build` as "resolve, verify
  hashes, link, type-check; writes/uses the lock".
- `book/src/packages-cli.md:76` says "Same lock => same bytes, same authority,
  offline."
- `rfcs/package-manager.md:574` documents `witchy build` as an offline build
  from lock that verifies every hash.
- `rfcs/package-manager.md:671` says `fetch` / `build` / `verify` reject records
  whose signature fails.
- `projects/pm/src/pm.witchy:143-159` implements `cmd_build` for project
  directories as build-step execution plus `witchy compile ... dep_flags(...)`.
  There is no call to `cmd_verify`, `verify_tuf`, `verify-rune`, `verify-vendor`,
  `hash_mismatches`, or `report_rune_verify`.
- `projects/pm/src/pm.witchy:132-141` implements project `run` by calling
  `run_project(...)`, not by verifying the lock first.
- `projects/pm/src/pm.witchy:176-199` implements `run_project` /
  `compile_then_run` as `witchy compile ... dep_flags(...) --out ...` followed by
  `witchy run sandbox ...`.
- `projects/pm/src/pm.witchy:222-253` implements `dep_flags` by reading
  dependency names from `witchy.lock` and mapping them to vendored source paths.
  It does not compare lock hashes to the source tree or verify adjacent
  `coven.json` records.
- `projects/pm/src/pm.witchy:1848-1907` contains the offline signed-record and
  source-hash verification logic in separate `verify-rune` / `verify-vendor`
  commands.
- BUG-047 already tracks that `witchy verify` itself does not match the offline
  provenance contract; this bug is the build/run path not enforcing the promised
  lock integrity before execution.

## Why this matters

The strongest package-manager claim is that a committed lock pins the bytes and
authority used by builds. If `build` and `run` skip lock verification, a local
edit under `vendor/<name>/src/` can change the code being compiled while the
lockfile remains unchanged.

This weakens the default user workflow. A separate verification command is useful
for CI, but the command named `build` is the one users naturally expect to enforce
the locked dependency identity before linking.

## Expected fix

Before project `build` or `run` compiles dependencies:

- verify path dependency hashes against `witchy.lock`;
- verify vendored registry `coven.json` signatures and source hashes against the
  pinned registry root key;
- reject if the lock references a dependency that is missing from `vendor/`;
- reject if `vendor/` contains a locked dependency whose source no longer matches
  the signed record; and
- decide whether online TUF freshness/root checks belong in build, or document
  build as offline-only with root keys pinned in the lock.

The fix should share one verification implementation with the eventual corrected
`verify` command, so `build`, `run`, and `verify` do not drift.

## Acceptance

- Tampering with `vendor/<name>/src/<name>.witchy` after `pm add` makes
  `pm build` fail before invoking the compiler.
- The same tamper makes `pm run` fail before executing the program.
- Tampering with `vendor/<name>/coven.json` makes `pm build` / `pm run` fail.
- A missing locked vendored dependency makes `pm build` / `pm run` fail.
- Tests cover path dependencies and registry dependencies separately.

## Resolution

Project build and run now call the same typed `LockIntegrityError` verifier as
the offline `verify` command before build steps or compilation. Path-only
projects materialize their first lock automatically, then enforce it on later
runs. Registry locks cannot be synthesized during build: they must come from
`add`, with the signed record and pinned root key. Vendored dependency discovery
also requires a `source = "coven"` lock entry, so changing the source kind cannot
turn an unverified directory into an executable dependency.

Focused tests cover automatic path locking, path drift, healthy offline build,
source and signature tampering, lock-coordinate substitution, missing root pins,
disguised source kinds, and missing vendored directories before compiler entry.
