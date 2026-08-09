# RFC-0120: Opt-in installs of staged (unreleased) versions in `pm`

- Status: Draft
- Author: pm / registry track
- Depends on: the staged/released lifecycle in `projects/coven` (publish → `Staged`,
  human promote → `Released`), RFC-0119 (human-gated release)

## Summary

`pm` resolves and installs **only `Released` versions**. This RFC adds a narrow,
loud, opt-in path to install a specific **`Staged`** version — one that CI has
published but a human has not yet released — behind an explicit flag, an
interactive `y/n` confirmation, and a persistent lockfile marker that re-warns on
every later install. The released-only default is unchanged.

## Motivation

A staged version is a real, registry-signed, content-addressed artifact that is
simply awaiting the human release gate (RFC-0119). Today there is no way to use one
without releasing it, which forces a false choice: to try a candidate build you must
either promote it (spending the human-release event, defeating the gate) or publish
a throwaway version. Every mature ecosystem lets you opt into pre-release artifacts
(`cargo`'s pre-release semver, `npm install pkg@next`, `pip --pre`). Concrete uses:

- **Verify before releasing.** Install `foo@1.4.0` while it is still staged, run it
  in a downstream project, and only then promote it.
- **Integration testing in CI** against a not-yet-released dependency build.
- **Coordinated multi-rune changes** where a consumer must test against a producer's
  staged version before both are released together.

## Why this is safe to allow (the usual objections don't apply here)

- **Reproducible.** Published versions are immutable (`coven`: *"already published —
  versions are immutable, bump the version"*), and a version can only transition
  `Staged → Released` or `Staged → Yanked`; its bytes and content hash never change.
  Pinning a staged version is therefore as reproducible as pinning a released one.
- **Integrity-preserving.** A `Staged` record is signed by the registry root key and
  content-hash-verified on fetch exactly like a released one. Installing staged is a
  *release-management* decision ("I accept an artifact its authors have not blessed
  for release"), **not** a supply-chain-integrity downgrade.
- **The data is already exposed.** `GET /coven/versions` returns every record with its
  `state`; `pm` filters to released client-side (`released_version_records`). So this
  is almost entirely a `pm` change — **no `coven` change is required** for the read
  path.

## Non-goals

- Changing the default. Ranges and bare `pm add name` stay released-only.
- Letting a staged version satisfy a **semver range**. Staged is always exact-version
  only (see below) — you can never *float* onto a staged version.
- Installing `Yanked` versions. Yanked stays refused, always.
- Any change to who may *release* (RFC-0119 is untouched).

## Design

### 1. Opt-in is per-version and exact only

Staged installs require BOTH an explicit flag and an exact version; a staged version
never satisfies a range, so nothing can drift onto one by accident.

```sh
pm add insanitybit/greeter@1.4.0 --allow-staged     # exact version required
pm add insanitybit/greeter@^1.4  --allow-staged     # ERROR: --allow-staged needs an exact version
```

This mirrors the existing `--allow-fresh` override (which lets a resolve accept a
released version still inside its staging-cooldown window). `--allow-staged` is the
analogous escape hatch for the `Staged` *state*. The two are independent: cooldown
applies to released versions; `--allow-staged` applies to the staged state.

Resolution change (`resolve_version` / `pick_version_cooled`): when `--allow-staged`
is set and `req` is an exact version, the candidate set is drawn from **all** records
for that exact coordinate (staged included), not just `released_version_records`.
Without the flag, or for any range, resolution is released-only exactly as today. A
`Yanked` record for that coordinate is still refused.

### 2. Interactive `y/n` confirmation (with a non-interactive escape)

Because a staged install is a deliberate, unusual act, an interactive `pm add
--allow-staged` prompts and requires an affirmative answer:

```
! insanitybit/greeter@1.4.0 is STAGED, not released — its authors have not blessed it
  for release. It is immutable and signed, but may never be released, or may be yanked.
  Install this unreleased version? [y/N]
```

- Uses the existing `Console.read_line()` builtin (already present; has a test input
  fixture for deterministic differential tests). Default (empty / anything but `y`)
  is **No**.
- **Non-interactive escape:** in CI a prompt must not hang. `--yes` (or
  `PM_ASSUME_YES=1`) skips the prompt. If stdin is not a usable prompt source and
  `--yes` is absent, `pm` refuses with a clear message rather than blocking. (Whether
  to detect non-interactive stdin, or simply require `--yes` whenever `--allow-staged`
  is used without a TTY, is an implementation detail resolved against the
  `read_line` fixture semantics; the safe default is: no `--yes` + no answer ⇒ refuse.)

### 3. Persistent lockfile marker — re-warn on every install

A staged pin is a **standing** property of the dependency, not a one-time decision at
`add`. The lockfile records that a pinned version was accepted while staged, and
`pm install` (which resolves from the lock, non-interactively) prints a warning for
each such dependency on **every** run:

```
! using STAGED insanitybit/greeter@1.4.0 (unreleased; accepted via --allow-staged)
```

- The lock entry carries an explicit `staged = true` (name to be finalized) marker on
  the pinned rune. `pm install` does not re-prompt (it is non-interactive by design),
  but it always re-warns, and it still verifies the signed record + content hash.
- **Self-healing:** if the pinned version has since been **released**, `pm install`
  drops the warning and clears the marker (the pin now resolves as a normal released
  version — same hash). If it has been **yanked**, `pm install` fails loudly (yanked
  is never usable).
- The vendored-record verifier (`VendoredRecordInvalidState`, which today hard-refuses
  any non-`released` state) is relaxed to accept `staged` **only** when the lock's
  `staged` marker is present for that rune; an unmarked staged vendored record is still
  corruption/refused.

### 4. Publishing still cannot depend on staged

A published rune must be self-contained and resolve only released registry deps
(existing `pm publish` rule). `pm publish` continues to **reject** a manifest whose
resolved dependency tree includes any staged pin — you may *consume* a staged version
locally, but you may not *ship* a rune that depends on one. This keeps the public
dependency graph released-only.

## Acceptance criteria

1. `pm add name@X.Y.Z --allow-staged --yes` installs a staged version; the same
   command without `--allow-staged` still reports "no released version satisfies".
2. `--allow-staged` with a range (not an exact version) is a hard error.
3. Interactive confirm: answering `n` (or empty) aborts without writing the lock;
   `y` proceeds. Verified with the `read_line` input fixture on **both** backends.
4. The lockfile marks the staged pin; `pm install` re-warns every run, verifies the
   hash, and does not re-prompt.
5. A staged pin that has since been released installs clean (no warning, marker
   cleared); one that has been yanked fails.
6. `pm publish` refuses a rune whose dependency tree includes a staged pin.
7. A `Yanked` version is never installable, with or without `--allow-staged`.
8. Differential parity: identical observable behavior (prompts, warnings, exit codes,
   lockfile bytes) on the interpreter and compiled-WASM backends.

## Alternatives considered

- **Pre-release semver tags instead of a state flag.** witchy/coven model release
  readiness as record *state*, not as semver pre-release identifiers; overloading
  semver would duplicate the staged/released lifecycle and complicate resolution.
  Rejected in favor of the existing state model.
- **A global `--allow-staged` that applies to ranges.** Rejected: floating onto a
  staged version is exactly the accident this design prevents. Exact-only.
- **No confirmation, flag alone.** Rejected: the user requirement is that this be
  hard to do by accident and visible every time; the prompt + persistent lock warning
  deliver that.
