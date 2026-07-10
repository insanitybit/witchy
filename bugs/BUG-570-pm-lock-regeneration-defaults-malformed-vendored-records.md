# BUG-570: PM lock regeneration defaulted malformed vendored records

Status: FIXED
Severity: HIGH
Component: `projects/pm`, vendored trust metadata, RFC-0054

## Summary

PM projected fields from each vendored `coven.json` through helpers that
returned `""` or `[]` for invalid JSON, missing/wrong-shaped strings, and a
malformed `runtime_footprint`. `pm add` and `pm update` then regenerated
`witchy.lock` from those defaults. A corrupt existing signed record could
therefore become a fresh lock entry with an alias substituted for its identity
and empty version, hash, or authority footprint.

The same defaulting let `add_rune` treat a malformed existing record as a valid
deduplication hit instead of reporting corrupt local trust state.

## Resolution

One local `VendoredRecord` projection now requires nonempty string `name`,
`version`, `state`, and `hash` fields plus an all-string
`runtime_footprint`. Version must be canonical SemVer, state must be
`released`, and hash must use the `sha256:` content-address form. Its
`VendoredRecordError` remains typed until the PM CLI renders the boundary
failure.

Lock serialization returns `Result` and callers write only after every
vendored record parses. Existing-rune deduplication and update consume the same
strict projection. End-to-end coverage corrupts an already-vendored
`coven.json` and verifies offline checking, update, and a later add/lock
regeneration all fail while the original lockfile remains byte-identical.
