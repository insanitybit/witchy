---
rfc: 0118
title: "Atomic Dir intrinsics: closing the SEC-049 coven store concurrency races"
status: implemented
created: 2026-08-08
tracking: "Spawned by RFC-0117 Lane C item 3 (the atomic-Dir-intrinsics RFC it promised to scope, not hand-wave). Grounded in scratch/audit-2026-08-08-registry/{trust-pipeline.md,SYNTHESIS.md}. Design-only; no implementation in this RFC."
---

# RFC-0118: Atomic Dir intrinsics — closing the SEC-049 store concurrency races

## Summary

The coven registry's only shared state across its worker VMs is an on-disk,
file-backed `Dir` capability, and the `Dir` cap surface today
(`crates/witchy-syntax/src/cap_ops.rs:168-178`) has **no atomic primitive** —
no exclusive-create, no atomic rename/replace, no lock, no atomic whole-file
write. Every `exists`-then-`write` in coven is therefore a genuine cross-process
TOCTOU race under `serve_pool` (one worker VM per core, no shared memory). Two
of those races are HIGH severity: concurrent requests can double-spend a token's
`jti` (defeating the SEC-022 replay defense) and can brick a content-addressed
version (interleaved writes leave `content_hash(files) != rec.hash`). This RFC
proposes a small set of **atomic filesystem primitives on the `Dir` capability**
— exclusive-create, atomic rename/replace, and an atomic-write helper built from
them — specified so they behave **identically on both backends** (the
interpreter oracle and the compiled-WASM/native host), and shows how coven uses
them to close the SEC-049 HIGH races. It specifies the primitives, the parity
contract, the coven callsite migration, and the test plan. **It does not
implement anything.**

This is the RFC-sized, parity-sensitive (WASM-ABI-touching) fix that the
2026-08-08 registry audit flagged as SEC-049's real remedy, and that RFC-0117
Lane C explicitly deferred to its own track.

## Motivation

### The substrate that makes the races real

`main → serve → server.serve_pool()` spawns **one worker VM per core**, each
accepting from the same listener, with **no shared memory**. The *only* shared
state is the on-disk `Dir` store (audit trust-pipeline.md:18, :81). That is
exactly what turns coven's check-then-write sequences from theoretical into
genuine cross-process TOCTOU: two requests land on two workers, both read the
same pre-state, both act on it.

The `Dir` cap surface offers only:

```text
only  list  read  exists  is_dir  subtree  make_dir  read_file  write_file  write  append
```

(`crates/witchy-syntax/src/cap_ops.rs:168-178`). None of these is atomic with
respect to a concurrent writer. `write` truncates-and-replaces in place;
`exists` is a separate call from the `write` that would follow it; there is no
create-if-absent-else-fail, no rename, and no lock.

### Threat model

- **Attacker capability.** Anyone who can issue two (or more) HTTP requests to
  the registry concurrently. In **anonymous** deployment mode this is any
  unauthenticated peer. In **trusted** mode it is any party holding one still-
  valid short-lived OIDC token (for the replay race, a *single* token suffices —
  the whole point is to spend it twice) or any authorized publisher (for the
  immutability race). No privileged position, no MITM, no host access required —
  just the ability to send two POSTs that arrive close in time.
- **What they defeat.** The registry's two core promises: *single-use tokens*
  and *immutable content-addressed versions*.

### The races, at the exact write patterns today

**(a) jti single-use replay — SEC-049 HIGH-1.** `consume_jti`
(`projects/coven/src/coven.witchy:490-501`) is the replay defense that publish
deliberately front-loads (`authorized_publisher` calls it at :486, *before*
`authorize_publish` at :487, so a replayed token can never bind namespace
trust). Its body:

```text
let marker = "_policy/_jti/${crypto.sha256(jti)}"
require(!self.store.exists(marker), 403, "identity token already used ...")?   // :498  READ
fs.ensure_dir(self.store, "_policy/_jti")
self.store.write(marker, "1")                                                   // :500  WRITE
```

Two concurrent requests carrying the **same** valid token land on two workers.
Both evaluate `!exists(marker)` as true (neither has written yet), both proceed,
both later `write` the marker. The token is **spent twice** — a double-spend
that defeats the SEC-022 ordering guarantee. Promote has the identical pattern
via `consume_jti` at `coven.witchy:437`.

**(b) content-address immutability / publish — SEC-049 HIGH-2.** The
immutability gate and the writes it guards are far apart and non-atomic:

```text
// do_publish
require(!coven_store.record_exists(self.store, name, version), 409, "... immutable ...")?  // :386  READ

// publish_checked, after validation
coven_store.store_source(self.store, name, version, files)   // :406  many WRITES (loop)
... sign_record ...
coven_store.write_record(self.store, name, version, rec)     // :408  WRITE coven.json
```

`store_source` (`projects/coven/src/coven_store.witchy:47-51`) writes each source
file one-by-one in a loop. Two concurrent first-publishes of the same
`ns/name@ver` both pass `!record_exists` at :386, then their per-file writes
**interleave** — the stored `rune/` tree becomes a mix of both uploads while
`coven.json` (`write_record`, coven_store.witchy:30-31) is last-writer-wins.
Afterward `content_hash(files) != rec.hash`, so `/coven/source` and `/coven/doc`
return 500 permanently: the version is **bricked** (denial-of-integrity from
unprivileged input in anonymous mode; from any authorized publisher otherwise).

**Adjacent same-shape races** (in scope for the primitives, not all separately
demonstrated here): the maintainer TOFU bind `bind_maintainer`
(`coven.witchy:548-556`, exists-then-write on `maintainers.json`, SEC-049 MED-4);
the publisher.json bind (`authorize_publish` :506-512); and the metadata roles
`rebuild_metadata → coven_meta.rebuild`, which `write`-overwrites
`snapshot.json`/`timestamp.json` in place so a concurrent reader or a mid-write
crash can observe a torn role (BUG-554 lineage).

### Why this is systemic, not a coven bug

Coven's logic is correct for a single writer. The defect is that the *language's*
`Dir` capability cannot express "create this marker if and only if it does not
already exist, atomically" or "replace this file atomically". No amount of
coven-side reordering fixes a missing primitive. The audit states it plainly:
"Fix requires new Dir intrinsics on both backends ... parity-sensitive wasm-ABI
additions, RFC-sized" (trust-pipeline.md:88, ledger row :54). Hence this RFC.

## Design

Three new operations on the `Dir` capability, plus one convenience built from
them. All are **write** operations (they require the same write authority
`dir.write`/`dir.write_file` already require) and all respect the existing
`DirPolicy` refinement (`dir.only(policy)`) and path-traversal guards unchanged.

### The primitives

Signatures are given in the cap-op table style used by
`crates/witchy-syntax/src/cap_ops.rs`. `Dir` is the receiver capability;
`Result` is witchy's standard result, so a caller uses `?`/`match`. Errors are
returned as a typed `DirError`, never a host trap (see Errors).

**1. `create_new` — atomic exclusive create.**

```text
dir.create_new(path: String, data: String) -> Result(Nil, DirError)
```

Atomically create `path` with the given contents **if and only if** it does not
already exist, in one indivisible step. If `path` already exists, fail with
`DirError::AlreadyExists` and do not modify the existing file. This is the
`O_EXCL | O_CREAT` semantics. There is no intervening observable state between
"does not exist" and "exists with `data`": a concurrent second `create_new` of
the same path is guaranteed to see exactly one winner and one
`AlreadyExists` loser. Parent directories must already exist (mirroring
`write_file`); missing parent → `DirError::NotFound`.

**2. `rename` — atomic rename/replace within the Dir.**

```text
dir.rename(from: String, to: String) -> Result(Nil, DirError)
```

Atomically move `from` to `to` within the same `Dir` authority. If `to` exists
it is atomically **replaced** (POSIX `rename(2)`/`renameat(2)` replace
semantics) — a concurrent reader of `to` observes either the whole old file or
the whole new file, never a torn or absent intermediate. Both paths are subject
to the Dir's policy and traversal guards; `from` missing → `DirError::NotFound`.
Cross-`Dir` rename is **not** offered (rename atomicity is only guaranteed
within one filesystem/authority); a caller that needs to move between authorities
uses read+`create_new`+delete explicitly.

**3. `try_lock` — advisory exclusive lock over a scope.**

```text
dir.try_lock(path: String) -> Result(Option(DirLock), DirError)
```

Attempt to acquire an advisory exclusive lock keyed by `path` (an `flock`-style
lock on a lock file). Returns `Some(lock)` if acquired, `None` if another holder
currently owns it (non-blocking `try`), `Err` on a real IO fault. The returned
`DirLock` releases on drop (RFC-0114 must-consume/obligation discipline applies —
a held lock is a consume-obligation so it cannot be silently leaked). This is the
**escape hatch** for multi-step critical sections that a single atomic op cannot
express (e.g. read-modify-write of `maintainers.json` as a set). The primary
races (a) and (b) are closed by `create_new`/`rename` **without** a lock; `try_lock`
exists so the design generalizes rather than special-casing the two known
callsites.

**4. `replace` — atomic whole-file write (convenience, derived).**

```text
dir.replace(path: String, data: String) -> Result(Nil, DirError)
```

Atomically replace `path`'s contents with `data`, creating it if absent,
last-writer-wins but never torn — a concurrent reader sees all-old or all-new.
Specified as sugar for "write to a unique temp sibling, then `rename` over
`path`", so it inherits `rename`'s atomicity. Provided because the metadata-role
writers (`snapshot.json`/`timestamp.json`) want "replace atomically" rather than
"fail if exists"; without it every callsite would re-implement the temp+rename
dance.

### Errors

A single typed error, returned (never trapped), so both backends map faults to
the same witchy value and coven can branch on it:

```text
DirError =
    AlreadyExists   // create_new: path already present (the race-loser signal)
    NotFound        // from/parent missing
    Denied          // policy / traversal / write-authority refusal
    Io(String)      // underlying host IO fault, message host-normalized
```

`AlreadyExists` is the load-bearing one: coven's jti-consume treats it as "token
already used", turning a lost race into the *same* 403 a sequential replay
already produces.

### Cap-op table additions

New rows in `crates/witchy-syntax/src/cap_ops.rs` alongside the existing Dir ops
(:168-178). All three primitives + `replace` are `Dir` receivers requiring write
authority; return types are the `Result`/`Option` shapes above. No change to
`only`/`list`/`read`/`exists`/`is_dir`/`subtree`/`make_dir`/`read_file`/
`write_file`/`write`/`append` — those keep their current signatures and
semantics.

## The parity contract (the prime directive)

Two backends, **zero silent divergence**. Each primitive must produce an
identical observable result — same success value, same `DirError` variant, same
effect on the store — on the interpreter and on the compiled-WASM/native host.
This section pins how.

### Where each backend implements it

- **Interpreter (oracle).** Dir ops are handled in
  `crates/witchy-interp/src/interpreter/builtins.rs` (the Dir match arms around
  :2100-2500), which dispatch either to a real filesystem backing or, under
  tests, to the fixture host via `HostRequest`
  (`crates/witchy-test-host/src/lib.rs:82`). New `HostRequest` variants —
  `DirCreateNew { dir, path, bytes }`, `DirRename { dir, from, to }`,
  `DirTryLock { dir, path }`, `DirReplace { dir, path, bytes }` — carry these to
  the fixture, mirroring the existing `DirWrite`/`DirMakeDir`/`DirAppend`
  variants. `HostResponse` gains the `AlreadyExists`/lock-handle shapes it needs.
- **Compiled-WASM/native host.** Dir ops are host functions in
  `crates/witchy-runtime/src/runtime/host/filesystem.rs` (`link_dir_write`,
  `host_dir_write`, `host_dir_make_dir`, `host_dir_append`, …). Each new
  primitive is a new host function + linker import (`host_dir_create_new`,
  `host_dir_rename`, `host_dir_try_lock`, `host_dir_replace`), guarded by the
  same `dir_require_write` + `dir_guard` (traversal) checks the existing writers
  use.

### The native syscall mapping (real host)

| Primitive      | Native syscall (openat-relative, respecting the Dir's dirfd) |
|----------------|--------------------------------------------------------------|
| `create_new`   | `openat(dirfd, path, O_WRONLY \| O_CREAT \| O_EXCL, 0o644)` → write → close. `EEXIST` ⇒ `DirError::AlreadyExists`. |
| `rename`       | `renameat(dirfd, from, dirfd, to)` (atomic replace). `ENOENT` ⇒ `NotFound`. |
| `try_lock`     | open/create a lock file under the Dir, `flock(fd, LOCK_EX \| LOCK_NB)`. `EWOULDBLOCK` ⇒ `None`; `DirLock` holds the fd and `flock(LOCK_UN)`+close on drop. |
| `replace`      | write to a unique temp sibling (`openat` `O_CREAT\|O_EXCL` with a random suffix), then `renameat` over `path`. |

All are **openat-relative to the Dir authority's dirfd**, so they inherit the
capability's confinement (no absolute paths, no escaping the granted subtree) —
identical to how the existing writers are rooted. The traversal guard
(`dir_guard`) runs on `path`/`from`/`to` before any syscall.

### The in-memory / fixture host (browser + tests)

The fixture and browser hosts back the Dir with an in-memory map, executed
single-threaded per host, so atomicity is trivially satisfied by making each
primitive one indivisible map mutation:

- `create_new`: check-and-insert as **one** operation on the map — if the key is
  present, return `AlreadyExists` without mutating; else insert. (The fixture
  host is not concurrently mutated, so "one indivisible map op" is the whole
  requirement; the contract is about the *observable result matching native*,
  and native's `O_EXCL` yields the same AlreadyExists-or-created result.)
- `rename`: remove `from`, insert at `to` in one op; `to` overwritten.
- `try_lock`: a lock-set in the host state; `try_lock` on an already-held key
  returns `None`. Because a fixture host is single VM, this is mostly for
  behavioral parity of the *return shape*; genuine cross-VM contention is a
  native-host concern.
- `replace`: last-writer-wins insert, always whole (a map insert is never torn).

The differential oracle is single-VM parity (per the determinism doctrine): the
interpreter and compiled backend must return the **same value** for the same
sequence of calls. Genuine multi-worker concurrency is exercised by the native
host in the e2e/concurrency test (below), not by the differential oracle.

### WASM ABI implications

Each primitive is a **new host function import** in the runtime linker (the
`link_dir_*` family in filesystem.rs), so this is an additive WASM ABI change:
new imported function signatures the guest calls, new `HostRequest`/`HostResponse`
variants for the fixture path. Return values cross the ABI as the existing Dir
ops do (status word + externref for the lock handle, byte buffers for paths).
Because it adds imports rather than changing existing ones, old guests keep
working; new guests that call the primitives require the new host. This is
exactly the kind of additive, parity-sensitive host change the standard
feature-stage gate governs (see Rollout).

### Error-mapping parity

The `DirError` variant returned for a given fault must be identical on both
backends: `EEXIST`/key-present ⇒ `AlreadyExists`; `ENOENT`/missing ⇒ `NotFound`;
policy/traversal refusal ⇒ `Denied`; anything else ⇒ `Io(msg)` with a
host-normalized message so the *variant* matches even if the OS string differs
(the differential test compares the variant, and coven branches on the variant,
never the message).

## How coven uses them to close SEC-049

Source-only callsite migration in `projects/coven/`, no logic redesign —
each check-then-write collapses into one atomic op.

**jti-consume (HIGH-1).** `consume_jti` (`coven.witchy:497-500`) replaces the
`exists`→`ensure_dir`→`write` sequence with a single exclusive create:

```text
let marker = "_policy/_jti/${crypto.sha256(jti)}"
fs.ensure_dir(self.store, "_policy/_jti")
match self.store.create_new(marker, "1"):
    Ok(_)                         -> Ok(jti)                       // we won: fresh token
    Err(DirError::AlreadyExists)  -> Err(CovenError(403, "identity token already used — single-use replay refused"))
    Err(e)                        -> Err(CovenError(500, ...))     // real IO fault
```

Now two concurrent consumes of one token: the kernel/host guarantees exactly one
`Ok` and one `AlreadyExists`, so the token is spent **once**. The 403 the loser
gets is byte-identical to the sequential-replay 403 already tested. Promote's
`consume_jti` at :437 inherits the fix (same function).

**publish immutability (HIGH-2).** Stage the version's record write behind an
exclusive create of the record path so exactly one first-publish wins the
coordinate, then write source under the now-owned coordinate:

```text
// claim the coordinate atomically FIRST (replaces the far-apart :386 gate)
match coven_store.claim_record(self.store, name, version):   // create_new(meta_path, "")
    Err(DirError::AlreadyExists) -> return Err(CovenError(409, "... already published — immutable ..."))
    Err(e)                       -> return Err(CovenError(500, ...))
    Ok(_) -> ()
// only the single winner reaches here; store source, then atomically finalize the record
coven_store.store_source(self.store, name, version, files)
coven_store.write_record_atomic(self.store, name, version, rec)   // replace(meta_path, rec)
```

`claim_record` uses `create_new` on `meta_path(name, version)`
(`coven_store.witchy:15-16`) to make the coordinate single-owner; the loser gets
the same 409 the `record_exists` gate produced. `store_source` still writes the
tree, but only one publisher owns the coordinate, so no interleaving of two
uploads. `write_record_atomic` uses `replace` so `coven.json` lands whole. Net:
`content_hash(files) == rec.hash` always holds; no bricking.

**Metadata roles (BUG-554).** `coven_meta.rebuild`'s `write` of
`snapshot.json`/`timestamp.json` becomes `replace`, so a concurrent reader never
sees a torn role.

**Maintainer / publisher TOFU binds (MED-4).** `bind_maintainer`
(`coven.witchy:548-556`) and the publisher.json bind (:506-512) use `create_new`
for the first-writer-wins bind; a lost race is a benign "already bound" rather
than a last-writer-wins clobber. (Note MED-3, the first-promoter-not-bound-to-
publisher *logic* hole, is a separate source-only fix per RFC-0117 Lane C item 2
and is **not** closed by these primitives — atomicity is necessary but not
sufficient there.)

## Test plan

Concurrency correctness cannot be shown by the differential oracle alone (it is
single-VM). Two tiers:

1. **Differential tests (both backends, single-VM) — behavioral parity of the
   primitives.** In `src/example_tests.rs`, for each primitive: `create_new` on
   an absent path succeeds and on a present path returns `AlreadyExists` without
   changing contents; `rename` moves and replaces; `replace` is whole-file;
   `try_lock` returns `Some` then `None` while held then `Some` after release;
   every `DirError` variant is produced by the matching fault. Each asserts the
   interpreter and compiled-WASM backends return the **identical** value. Plus a
   `book/` example demonstrating `create_new` as a single-use marker (a complete,
   runnable program).

2. **Native concurrency e2e — the tests that FAIL today, PASS after.** In
   `tests/e2e/` (alongside `trust_and_publishing.rs`), against a real
   `serve_pool` registry on the native host:
   - **Parallel jti double-spend:** fire N concurrent publishes carrying the
     **same** valid token at the same coordinate-adjacent namespaces. *Today:*
     more than one succeeds (token spent >1×). *After:* exactly one succeeds; the
     rest get 403 "already used". Assert the jti marker exists once and only one
     namespace bind occurred.
   - **Parallel same-version publish:** fire N concurrent first-publishes of the
     **same** `ns/name@ver` with different source bytes. *Today:* the stored tree
     is a mix and `content_hash != rec.hash`, so a subsequent `GET /coven/source`
     or `/coven/doc` 500s (version bricked). *After:* exactly one publish wins
     (200), the rest 409, and `content_hash(stored) == rec.hash` (source + doc
     serve cleanly).
   - **Parallel metadata rebuild:** concurrent publishes racing
     `rebuild_metadata`; assert no reader ever observes a non-decoding
     `snapshot.json`/`timestamp.json`.

   These are the first concurrency tests in the coven suite (the audit notes all
   current publish/promote tests are sequential — trust-pipeline.md:86); they are
   the acceptance evidence for SEC-049 HIGH-1/HIGH-2.

## Alternatives

- **A global registry mutex / single-writer serialization.** Serialize all
  mutating requests through one lock (or one dedicated writer VM). *Rejected as
  the primary fix:* it throws away `serve_pool`'s per-core parallelism for every
  write, converting an availability-constrained service (audit P0: workers =
  cores, 1–2 on a small instance) into a single-writer bottleneck, and it is a
  coven-specific band-aid that leaves the *language* unable to express atomic
  filesystem effects — the next Dir-backed program hits the same wall. It also
  does not survive multiple registry processes/instances (a mutex is in-process;
  the disk is the real shared state). The atomic primitives push correctness down
  to the substrate that is *actually* shared (the filesystem), so they hold for
  any number of workers or processes and generalize to every Dir user. (`try_lock`
  is offered for the genuine multi-step critical sections, so the "I need a lock"
  case is still served — just scoped, not global.)
- **A coven-level lockfile via the existing `write`/`exists`.** Cannot be made
  correct — building a lock out of non-atomic `exists`+`write` reintroduces the
  very TOCTOU it is meant to remove.
- **Content-addressed store keyed purely by hash (dedup) so re-writes are
  idempotent.** Helps immutability (identical bytes → identical path) but does
  **not** help the jti replay race (a marker is not content) nor torn metadata,
  and it is a larger storage-model change. Orthogonal; could layer on later.
- **Do nothing / mitigate at the edge.** The edge (Fly proxy) cannot serialize
  application-level check-then-write races; two requests that both pass the proxy
  still race on disk. Not a fix for HIGH-1/HIGH-2.

## Drawbacks

- **WASM ABI surface grows.** Four new host functions + `HostRequest`/`Response`
  variants across three crates (`witchy-syntax` cap table, `witchy-interp` +
  `witchy-test-host` fixture path, `witchy-runtime` native host). Additive, but
  every new host intrinsic is permanent surface under the parity discipline.
- **`try_lock` adds a resource with a lifetime** (`DirLock`), which leans on
  RFC-0114 must-consume obligations to prevent leaked locks; that coupling is
  real complexity, and it is why the two HIGH races are closed with the
  lock-free `create_new`/`rename` and `try_lock` is reserved for multi-step
  cases.
- **Native lock semantics are advisory** (`flock`) and process-local to one
  host; they do not coordinate across separate machines/NFS. This is adequate for
  coven's single-instance-with-N-workers model but must be documented, not
  assumed to be a distributed lock.
- **No new per-method fast paths.** These are general capability operations, not
  optimizations; they must not grow the `*_cap`/`self_*` zoo (CLAUDE.md). They
  are new *semantics*, implemented once per backend, consumed uniformly.

## Rollout

Because this touches the WASM ABI and both backends, it lands behind the
project's standard parity discipline:

- **One feature-stage gate before both backends** — the frontend (cap-op table,
  typeck of the new signatures/`DirError`) is gated so the primitives cannot be
  used until *both* the interpreter and the compiled host implement them
  identically; no interpreter-only interval where the compiled path traps.
- **Land order.** (1) Frontend + `DirError` type + cap-op rows (gated off).
  (2) Interpreter + fixture-host implementation and the differential tests.
  (3) Native host functions + linker imports and the native concurrency e2e.
  (4) Flip the feature-stage gate on once (2) and (3) are green and the
  differential suite adjudicates parity. (5) Coven callsite migration
  (`consume_jti`, publish claim/finalize, metadata `replace`, TOFU binds) + the
  e2e that proves SEC-049 HIGH-1/HIGH-2 closed.
- **Gate discipline.** Every step goes through `./scripts/check.sh --fast` in a
  worktree, then the merge queue's full gate; the parity sweep and e2e in CI are
  the backstop. The coven migration (step 5) is source-only and rides the
  existing coven e2e plus the new concurrency e2e.
- **Coordination with RFC-0117.** RFC-0117 Lane C item 3 scoped this and hands it
  off; Lane C items 1 (SEC-048) and 2 (MED-3 promoter binding) are independent
  source-only fixes that do not wait on these primitives.

## Prior art

- POSIX `open(2)` `O_EXCL|O_CREAT`, `rename(2)`/`renameat(2)` atomic-replace,
  `flock(2)` — the canonical atomic-filesystem building blocks these primitives
  wrap.
- The write-to-temp-then-`rename` "atomic file replace" idiom (used by editors,
  package managers, and databases for crash-safe writes) — the basis for
  `replace`.
- TUF's requirement that metadata role updates be observed atomically informs the
  `snapshot.json`/`timestamp.json` `replace` migration (coven's `coven_meta`).
- Internal: `scratch/audit-2026-08-08-registry/{trust-pipeline.md,SYNTHESIS.md}`
  (the SEC-049 root-cause and the concurrency characterization); RFC-0117 Lane C
  (the deferral that spawned this); RFC-0114 (must-consume obligations, for the
  `DirLock` lifetime); the determinism doctrine (single-VM differential oracle;
  multi-VM concurrency proven by native e2e, not the oracle).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->

## Change notes

### 2026-08-09 — implemented (scoped to the lock-free primitives)

Shipped the three **lock-free** primitives that close the SEC-049 **HIGH** races,
implemented identically on both backends and proven at parity:

- `dir.create_new(path, data) -> Bool` — atomic `O_CREAT|O_EXCL`; `true` = this
  call created it, `false` = it already existed (the race-loser signal).
- `dir.replace(path, data) -> Nil` — atomic whole-file replace (temp sibling +
  `renameat`).
- `dir.rename(from, to) -> Nil` — atomic `renameat` replace within the Dir.

**Deviations from the design above, and why:**

- **Return shape is `Bool`/`Nil`, not a typed `DirError`.** The load-bearing
  signal is `AlreadyExists`, delivered as `create_new`'s `false` (coven branches on
  it exactly as the design's `Err(AlreadyExists)` → 403/409). witchy's entire
  existing `Dir` surface already *traps* on a genuine IO/denied fault (coven's
  `store.write` was unchecked), so trapping there is zero regression — and it avoids
  a whole new error-ABI across both backends for the merely-exceptional IO case. A
  typed `DirError` can layer on later without changing these callsites.
- **`try_lock`/`DirLock` deferred.** As this RFC states, both HIGH races close
  without a lock; `try_lock` is the multi-step-critical-section generalization and
  depends on RFC-0114 must-consume obligations. Deferred to a follow-up rather than
  block the security fix on the obligation machinery.

**Where it landed.** The atomic FS semantics live once, in the shared
`crates/witchy-runtime/src/confine.rs` (`ConfinedDir::{create_new,replace,rename}`),
which *both* backends already use (the interpreter's `DirValue::Fs` IS a
`ConfinedDir`), so the two backends cannot diverge on the filesystem effect. Wiring:
cap-op table (`cap_ops.rs`), typeck (`capability_calls.rs`), interpreter dispatch
(`builtins.rs`), the WIR prelude/classifiers/helper-registry, codegen lowering, the
native host (`host/filesystem.rs`), and the fixture host for both the interpreter
(`witchy-testkit`) and compiled-fixture (`host/fixture.rs`) paths. Differential
coverage is a `book/src/cookbook-files.md` runnable example + `test_*` (both
backends, fixture host) and a `confine.rs` unit test (native FS).

**coven migration (SEC-049).** `consume_jti` now uses `create_new` (HIGH-1: the
jti double-spend is closed — exactly one winner). Publish claims the version
coordinate with `coven_store.claim_record` (`create_new` an empty placeholder)
before writing source and finalizes with an atomic `write_record` replace (HIGH-2:
concurrent same-version publishes can no longer interleave source and brick the
content hash); `all_target_digests` skips a mid-publish placeholder. `coven_meta`
role writes use `replace` (BUG-554 torn writes). The publisher/maintainer TOFU
binds use `create_new` first-writer-wins (MED-4). The genuine multi-worker
concurrency e2e (the design's "tests that FAIL today, PASS after") remains a
follow-up — the differential oracle is single-VM.
