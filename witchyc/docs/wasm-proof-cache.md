# Proof cache

```
WITCHY_PROOF_CACHE=/absolute/dir witchyc wasm-checked program.wy -o program.wasm
scripts/run-fixtures.py ... --proof-cache DIR FIXTURE.wy...
```

With `WITCHY_PROOF_CACHE` set to an existing absolute directory, and a checker
binding selected (`WITCHY_WASM_READY_BINDING`), a proof obligation the checker
has already accepted is not sent to Lean again.

Measured on `tests/fixtures/references/elements.wy` (75 obligations, imports
the Vec and String libraries): 8.5 s cold, 0.05 s warm, identical Wasm. A
program that adds one new proved function to cached libraries pays for one
request: about 4 s, nearly all of it starting the helper and hashing the Lean
installation, not the proof.

Without the variable nothing changes: every obligation is checked afresh, and
the fixture runner still insists on that unless given `--proof-cache`.

## What an entry means

One entry per obligation, keyed by a SHA-256 of:

- the checker binding record (helper hash, SDK hash, helper source hash);
- the exact Lean module that states and proves that one obligation: imports,
  prelude, statement, proof script and axiom audit;
- for an imported function, the verification model and the source of its defining module and the sources
  it imports transitively.

A library entry is reused across application edits that preserve its exact
Lean obligation. Editing a library or a transitive dependency invalidates
that scope, even when the edit leaves the proof text identical. This coarse
invalidation includes comments and private declarations. An identical
theorem in another library scope has its own evidence key. Cache hits still
require the selected checker binding to validate before publication.

The entry records which of the three allowed axioms the proof used. A changed
statement, proof script, prelude, audit, helper or SDK record gives a different
key, so it is a miss. An entry that is malformed or lists an
axiom outside the allowlist is a miss.

An entry is written only after the request containing the obligation passed
the kernel and the per-theorem axiom audit. A failed request writes nothing.

Obligations are independent theorems over the prelude, so one proved inside a
request with others is proved alone as well: the entry does not depend on what
else was in the request.

## On disk

One file, `accepted.v1`, with a line for each accepted goal: the key, a space,
`axioms=` and the names the audit found. A build reads it once. Storing appends
whole lines in one write. A line that is not exactly an entry (wrong key
length, a name outside the allowlist, anything extra) is skipped, so that goal
is checked afresh.

Until 2026-11-15 each entry was its own file, and a cached build opened one
file per goal: 76 opens, 40% of the build's time for a program that uses the
library. Directories in that layout are still read; each entry found is copied
into `accepted.v1`.

## What the receipt says

Each obligation has an `outcome`: `fresh_kernel_checked` or
`cached_kernel_checked` (with the recorded `axioms`). The receipt gains

```
"proof_cache": {"schema": "witchy.proof-cache.v1", "checker_binding_sha256": "...",
                "hits": 75, "misses": 0, "trust": "cache_directory_integrity"}
```

When every obligation is a hit, no helper is started and the Lean installation
is not hashed, so the receipt has no fresh-check block.

## Trust

The cache directory joins the trusted base. Anyone who can write a file there
can make the compiler accept an unproved obligation, exactly as anyone who can
replace the compiler or the helper can. Nothing is signed. That is why the
cache is opt-in and why receipts say which obligations came from it. A release
build should run without it.

Since 2026-11-21 the compiler checks that the writers are the user running it
and nobody else, and refuses the build otherwise:

- the directory is owned by the effective user, has no group or other write
  bit, and no component of its path is a symbolic link (on macOS name
  `/private/tmp/...`, not `/tmp/...`);
- on macOS the directory carries no access control list (`chmod -N` removes
  one), and neither does a file below; each directory above it belongs to root
  or the user and is writable by nobody else unless it is sticky;
- `accepted.v1`, `refused.v1`, an entry in the earlier layout and a lock file
  are regular files with the same owner and modes, never links. New files are
  made mode 0600.

The directory is opened once, component by component without following links,
and every file is opened relative to that handle and checked on the handle that
is read or written, so replacing the directory or a file between a check and
its use gains nothing. Root, and anyone who can act as the user, are trusted as
before.

A cached proof was checked against the SDK the binding names at the time; the
warm build does not re-hash the SDK.

## Not done

Reuse of an installed generic body (`kernel_typed_body_application`) is turned
off while the cache is on; the two would need a joint key. No eviction: delete
the directory to clear it.
