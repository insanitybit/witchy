---
rfc: 0076
title: "Capability ops are methods: one spelling for the effect surface"
status: planned
created: 2026-07-08
# Accepted 2026-07-08. Sequencing note: land after the RFC-0072 golden harness
# covers the cap-op diagnostics (the new bare-form rejection message and the
# sweep both want the net), and after the BUG-216-class dispatch fixes soak.
tracking: quality-audit follow-on (scratch/audit-2026-07-07-quality/REPORT.md discussion); executes RFC-0070 rule 2 for the cap-op verbs
related:
  - "0046 (typed trait dispatch — cap_op_return_type is the residual this shrinks)"
  - "0050 (method-call generalization — the receiver-typed dispatch this rides on)"
  - "0057 (capability policy constructors — Net.*/Dir.* statics; unaffected, complementary)"
  - "0067 story 2 (public APIs are the language surface)"
  - "0070 D5/D8 (nothing shipped twice; fmt is the migration vehicle)"
  - "0072 (diagnostic goldens — the net under the new rejection message)"
---

# RFC-0076: Capability ops are methods

## Summary

Capability operations — `read`, `write`, `connect`, `listen`, `print`, and the
rest of the effect verbs — currently have **two spellings**: a bare global call
(`print(console, "hi")`, `read(dir, path)`) and the receiver-method form
(`console.print("hi")`, `dir.read(path)`). Both type-check today; the docs
teach only the bare form (171 sites in `book/` vs 0 method-form). This RFC
makes the **method form the only spelling**: the bare global form becomes a
check-time error with a fix-it hint, all docs/std/projects migrate in one cut,
and the intrinsic substance — checker-owned signatures, rights enforcement,
host-import lowering — is explicitly retained unchanged.

Capabilities then read like every other type: operations live on the receiver,
and the value's type tells you what you can do with it.

## Motivation

**The inconsistency.** Every stdlib function is spelled `module.fn(args)` or
`receiver.fn(args)` — witchy deliberately has no bare global functions
("stdlib, not builtins"). The cap ops are the lone exception: they are bare,
unqualified, *global* names. This is not the documented module/method duality
(there is no `fs.read` module form — the bare form is qualified by nothing),
it is a third spelling that exists only for the effect verbs. RFC-0070 rule 2:
one spelling per concept, achieved by deletion.

**The namespace squat.** `read`, `write`, `connect`, `listen`, `resolve`,
`print` are premium identifiers, globally reserved today. Under method-only
spelling they are reachable solely in method position on a capability-typed
receiver; user code gets the bare names back.

**The pedagogy.** "Authority is a value; the type tells you what it can do" is
the language's thesis. `console.print("hi")` *shows* the thesis — the
operation is visibly an action of the capability. `print(console, "hi")` reads
like an ambient global that happens to take a console. The narrowing chapter
already drifted to the method form (`dir.read_file("config.toml")`,
`book/src/capabilities-narrowing.md:131`) because it reads better; the rest of
the book never caught up.

**The dispatch residual.** The RFC-0046 cleanup left one documented string
residual precisely because cap ops are bare intrinsics the empty TypeTable
cannot surface: `cap_op_return_type` (chained cap-op results). One spelling in
receiver position gives the checker a uniform place to type these calls, which
shrinks that residual's surface (it does not remove the pre-mono pass; see
Non-goals).

**What was measured** (2026-07-08, master):

- `book/`: 171 bare `print(console, …)`, 19 bare `read(`/`write(`, 0
  method-form `console.print`;
- `projects/`: 190 bare `print(console, …)`;
- `std/`: 2 bare sites (`std/fs.witchy` uses bare `read(root, child)`);
- both spellings verified type-checking on master (probe:
  `dir.read("config.toml")` / `console.print("hi")` → ok).

## Design

### 1. Canonical form

Every capability op is spelled as a method on its capability receiver:

```sh
console.print("hello")
let text = dir.read("notes.txt")          # Dir op: (path) — needs Dir[Read]
let cfg  = dir.read_file("config.toml")   # yields File[Read]
let body = f.read()                       # File op: no path — needs File[Read]
let sock = net.connect("example.com:443")
let quiet = net.only(policy)              # narrowing ops are methods too
```

A pleasant side effect: the `read` arity disambiguation (File `read(f)` vs Dir
`read(dir, path)`, today resolved by argument count in `check_file_op`)
becomes receiver-typed and self-evident — `f.read()` vs `dir.read(path)`.

### 2. The bare form is deleted, not deprecated

A bare call to a cap-op name (`print(...)`, `read(...)`, …) that is not
resolved by an in-scope user/std function of that name is a **check-time
error** with a fix-it:

```
type error: `main`, line 3: `print` is a capability operation and is spelled
as a method — write `console.print("hi")`
```

No alias period, no warning phase (break-don't-deprecate; there are zero
external users). The diagnostic lands with a golden in the RFC-0072 harness.

User code may freely declare its own `fn read(...)` etc. after this — the
names are no longer globally reserved. (Shadowing today is already possible;
this makes the rule simple: bare name = ordinary function resolution, method
on a cap-typed receiver = cap op. The two can no longer collide.)

### 3. What does NOT change

- **The intrinsic substance.** Signatures and rights checks stay
  checker-owned (`check_file_op` / `check_dir_op` / `check_net_op` /
  console/clock/etc. tables in `crates/witchy-types/src/typeck.rs`); lowering
  to host imports and interpreter natives is untouched. Cap ops do **not**
  move into std, and no FFI/extern declaration form is introduced.
- **The closed set.** The op inventory is still the complete effect surface,
  1:1 with grantable host imports; deny-by-omission and PM footprint
  computation are unaffected (both key off host imports, not source spelling).
- **RFC-0057 policy constructors** (`Net.only_hosts(…)`-style type-qualified
  statics) — different concept (constructing policy values, not performing
  effects), already type-qualified, unchanged.
- **`main`'s import-free hello world.** Method calls need no import;
  chapter 1 becomes `console.print("hi")` with no new ceremony.

### 4. Migration (the cut)

Inventory source of truth: the checker's op tables (every name/arity in
`check_*_op` and the console/clock/rand/env/exec/secret tables). For each op,
in one release batch:

1. **Checker**: accept method position only; bare position produces the
   fix-it error above. (Mechanically: the bare-call paths into `check_*_op`
   are removed; the method-call path — which already exists — remains.)
2. **fmt as the vehicle** (RFC-0070 D8): `witchy fmt` rewrites bare cap-op
   calls to method form (`print(console, x)` → `console.print(x)`); the sweep
   over `std/`, `book/`, `examples/`, `spec/`, `projects/` is then mechanical.
   If D8's round-trip fidelity has not landed, the sweep is done by targeted
   rewrite instead — the pattern is regular; fmt is preferred, not required.
3. **Docs**: the book/spec teach only the method form; the spec's capability
   section documents the rule in one line ("capability operations are
   methods; there is no bare form") and the effect-surface inventory table
   gains the method spelling.
4. **Goldens**: the new rejection diagnostic + one golden per op family; the
   existing 171+ executed book fences re-run under the new spelling (the
   executed-docs discipline is the migration's own regression net).

### Verification

- `./scripts/check.sh --fast` while iterating; the full gate via
  `./scripts/merge-queue.sh submit <branch>` (never two full gates at once).
- Differential tests: one per op family in `src/example_tests.rs` exercising
  the method spelling on both backends; `witchy parity` on a representative
  program. `witchy <file>` runs the compiled backend only — divergences are
  checked with the parity tool, never a single-backend run.
- Landmines: never `cargo fmt` (Rust is hand-formatted); `spec/stdlib.md` is
  generated from `std/*.witchy` doc-comments (regenerate after the std sweep:
  `witchy doc std/*.witchy > spec/stdlib.md`); **no new `*_cap`/`self_*`
  per-method fast paths** — this RFC changes spelling resolution only, not
  optimization paths.

## Alternatives

- **Docs-only canonicalization (keep the bare form legal).** Rejected: ships
  the concept twice forever (RFC-0070 rule 2), keeps the global namespace
  squat, and the corpus would drift back — the 171/0 book split happened
  precisely because both forms were legal.
- **Move cap ops into std as declarations.** Rejected: requires a
  body-less/extern function form (FFI surface the language deliberately
  defers), and a std wrapper cannot own rights semantics anyway — the checker
  would still special-case these names; the module home would be a fiction.
- **A `cap.` pseudo-module (`cap.read(dir, path)`).** Rejected: invents a
  fourth spelling to remove a second one; receiver-method form already exists
  and is the one that teaches the model.
- **Do nothing.** Rejected: audits keep re-finding the dual spelling; the
  book teaches the form that reads worst for the language's own thesis.

## Drawbacks

- **The biggest mechanical sweep since the stdlib cut**: ~380+ call sites
  across book/projects/std/examples. Mitigated by regularity (one rewrite
  pattern), the executed-fence tests, e2e suites, and fmt as the vehicle;
  composes with RFC-0071's dogfood sweep (same files, same batch is allowed
  but not required — 0076's rewrite is independent and simpler).
- **Grep-ability shifts**: the effect surface is now grepped as `\.read\(`
  rather than `^read\(` — weaker as a text pattern. Acceptable: the *load-
  bearing* inventories (host-import catalog, capability footprints) are
  computed from compiled artifacts, not source grep.
- **Method-call dispatch becomes load-bearing for effects**: a receiver whose
  type the checker cannot determine now fails to find cap ops. This is
  RFC-0046's typed path working as designed — the type error guides the user
  to annotate — but any residual dispatch gaps (BUG-216 class) get more
  exposure. The differential/golden coverage in §4.4 is the guard.

## Prior art

- Every mainstream capability-flavored design (E, Pony, Cap'n Proto RPC)
  spells authority use as messages/methods on the capability value — the
  receiver *is* the authority.
- Go/Rust file APIs (`f.read()`, `File::read`) — the receiver-method form is
  what practitioners already expect; witchy's bare form is the anomaly.
- This repo's Phase-2 "builtins become the stdlib — one cut, no aliases"
  (91f2122): the same deletion executed once already, successfully, at larger
  scale.
