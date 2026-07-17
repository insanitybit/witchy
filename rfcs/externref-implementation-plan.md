---
rfc: 0005-impl
title: Externref capability core — implementation design (companion to RFC-0005)
status: in-progress
created: 2026-07-03
tracking: rfcs/0005-unforgeable-capabilities.md
---

# RFC-0005 externref core — implementation design

> Companion to `rfcs/0005-unforgeable-capabilities.md`. That RFC decides the
> *what* (capabilities become `externref`, not `i32`) and chooses representation
> **(A) GC structs** for cap-carrying aggregates. This document is the *how*: the
> concrete migration for the compiled backend, staged so each stage is reviewable
> and — where possible — independently green. The plan now doubles as the
> implementation ledger: completed stages below describe shipped code, while
> unfinished stages retain their acceptance criteria.
>
> Written design-first at the maintainer's request, then updated as the staged
> ABI migration landed. Each capability kind still moves atomically across its
> producer, consumer, and aggregate boundaries.

## 1. What is already true (so the cut is smaller than it looks)

Two facts shrink the work and de-risk parity:

1. **The interpreter is already at the target.** `witchy-interp` holds
   capabilities as ordinary Rust enum values (`Value::Net(..)`, `Value::Dir(..)`,
   `Value::Secret(..)`) that a guest cannot fabricate. It has no i32 handle table
   and no linear memory for caps. So the externref core does **not** touch the
   interpreter — it raises the *compiled* backend to the interpreter, closing a
   parity gap rather than opening one. Every differential test's oracle already
   reflects the post-cut semantics.
2. **Grant-scope enforcement is unchanged.** The host import bodies
   (`host_dir_read_len`, `host_net_connect`, `host_crypto_sign`, …) already apply
   the real scope checks (`confine::resolve`, `address_admits`, the secret-table
   lookup — now with no `signing@0` magic, per step 1). Only the *naming* of the
   capability changes: an unforgeable `externref` replaces a forgeable `i32` the
   host resolves by table index. The checks move from "index `VmState.nets[h]`"
   to "downcast the `externref` to its backing `Arc<Grant>`" — same authority,
   same code.

So the cut is concentrated in exactly two places: (a) the compiled ABI at the
host boundary, and (b) the representation of any *value shape that transitively
contains a capability*. Everything else — the scalar heap, the vast majority of
records/lists/dicts/strings — is untouched.

## 2. Confirm the representation: (A) GC structs, scoped

RFC-0005 recommends (A) over (B) side-tables because (A) leaves **no corruption
path at all**: the reference is stored directly in a GC struct field, never as an
index a corrupted linear-memory word could swap for a fuller grant the instance
also holds. This plan adopts (A), scoped to the **cap-carrying subset** of value
shapes so the linear-memory heap (all non-cap values) is unchanged.

Concretely, wasmtime enables the GC proposal by default (the Config already
leaves `reference_types`/`gc` on — RFC-0005 step 7 disabled only the proposals we
don't emit, keeping these two for exactly this cut). The browser story is noted
as an open risk in §8.

## 3. Which value shapes are "cap-carrying" (the static classification)

The lowering needs a predicate `carries_cap(ty) -> bool`, computed over resolved
types (post-`typeck`), true iff a value of `ty` can transitively hold a
*guest-represented, handle-bearing* capability:

- a **guest-represented handle** capability type — `Dir`, `Net`, `File`,
  `Secret`, the derived `Socket`/`Listener`, and every `capability`/`grantable
  capability` declaration that wraps one of those handles (incl. the RFC-0002
  branded forms like `Redis(net)` and the RFC-0039 UI tokens);
- a record/`type` variant with any cap-carrying field;
- a tuple with any cap-carrying element;
- a closure whose captured environment has any cap-carrying capture;
- `Option`/`Result`/`List`/`Dict` **only** if their element/payload is
  cap-carrying (a `List(Int)` is not; a `List(Net)` is — rare but must be handled
  or explicitly rejected, see §7).

**Zero-representation capabilities are excluded.** `Console`, `Clock`, `Rand`,
`Env`, `Exec`, and `SecretStore` do not have a guest-held authority handle to
corrupt. Codegen drops their value argument (or ignores the placeholder) and the
host import itself is the authority gate: e.g. `host_print` takes only `ptr/len`,
`rand.rand_u64()` and `env.get_env(name)` take no guest cap operand, `exec.run`
drops the `Exec` argument and confines through its `Dir`, and `SecretStore` is a
root authority for `secretstore_lookup` rather than a guest index. A capability
with no bytes in linear memory is already unforgeable: there is nothing to
corrupt, swap, or mint. So `carries_cap` is false for these zero-representation
caps and they are absent from the migration (§6); the cut only touches
capabilities that currently name authority with guest-represented handles.

`carries_cap` is monomorphization-time: generics are already specialized before
codegen, so the concrete element types are known. The predicate is a small
recursive walk with a cycle guard (recursive `type`s), memoized per type id.

**Design rule:** a value shape is lowered to a **GC struct** iff `carries_cap`
holds for it; otherwise it stays in linear memory exactly as today. The bulk of
the heap never changes representation.

## 4. The dual representation in WIR + codegen

### 4.1 WIR surface
- New `WirTy` variants: `Extern` (a bare `externref`, for a capability value in a
  local/param/global) and `GcRef(struct_id)` (a typed `(ref $s)`). The encoder
  gains a **type section** emitting `struct` defs for each cap-carrying shape and
  wires `struct.new` / `struct.get` / `struct.set` / `ref.null` opcodes.
- New `WirExpr`/`WirNode`: `StructNew{struct_id, args}`, `StructGet{struct_id,
  field, base}`, `StructSet{struct_id, field, base, value}`. These mirror the
  existing linear-memory `Load`/`Store` but target GC fields; the field index
  replaces the byte offset.
- `heap_base`/`rc_alloc` are untouched — GC structs are allocated by the wasm GC
  runtime, not the linear-memory bump allocator, so the rc-floor reclamation
  (RFC-0016) applies only to the linear-memory heap. Cap-carrying GC structs are
  reclaimed by wasmtime's GC. (Interaction risk: §8.)

### 4.2 codegen lowering
- **Cap value in a local/param:** `WirTy::Extern`. A `main` param `net: Net`
  becomes an `externref` param the host populated at instantiation.
- **Cap-carrying record construction** (`Redis(net)`, `Server(net)`, a UI token,
  a struct literal with a cap field): lower to `StructNew` with a GC struct type
  whose fields are the cap fields (as `externref`) plus any scalar fields (as
  their normal wasm types, now GC struct fields rather than linear-memory words).
- **Field access / match on a cap-carrying record:** `StructGet` (by field index)
  instead of `Load` (by byte offset). `match` binding of a sealed capability's
  fields (`UiRoot(_) -> UiFetch(...)`) lowers to `StructGet` on the struct.
- **Closures capturing a cap:** every function value is a uniform GC wrapper.
  Each boxed lambda stores all captures in a per-lambda typed GC struct and
  recovers that payload through a checked `ref.cast`; scalar-only and
  capability-bearing closures therefore share one first-class representation.
  `CallIndirect` receives the wrapper as its exact leading parameter.

### 4.3 host imports
Each capability host import changes signature from `(handle: i32, …)` to
`(cap: externref, …)`. `IMPORT_COUNT` and the `wir_prelude` signatures update in
lockstep. The host body downcasts the `externref` to the backing grant
(`Arc<DirGrant>` / `Arc<NetGrant>` / `Arc<Secret>` …) via
`ExternRef::data`/`downcast_ref`, then runs the unchanged scope check. `VmState`
drops `dirs`/`nets`/`secrets` as index tables; grants are held alive by the
`externref`s the host minted into the instance.

### 4.4 The i64 Slot boundary (reject-first)

witchy boxes any *dynamically-typed* value into an **i64 Slot**
(`WirTy::Slot => Kind::I64`, `crates/witchy-wir/src/wir.rs`), moving in and out
with `ToSlot`/`FromSlot`. An `externref` has no i64 bit-pattern — that
unforgeability is the whole point — so a cap-carrying value **cannot** round-trip
through a Slot. Every `ToSlot`/`FromSlot` crossing of a cap-carrying value gets an
explicit **reject-or-represent** decision. The default is **reject-first**,
symmetric with the §7 collections decision: forbid the crossing in `typeck` in the
first cut, lift specific cases to a represented (GC-typed) form only when a real
program needs it.

- **Generic calls / type-var params boxed to Slot — REJECT.** A `fn id(x: a) -> a`
  monomorphized at `a = Net` would box the `Net` to a Slot. `typeck` rejects a
  handle-bearing capability as the argument of a Slot-boxed generic parameter.
- **`Option`/`Result` payloads — REJECT (first cut).** The `Some`/`Ok` payload is
  widened to a Slot today, so `Option(Net)` etc. cannot survive it. `typeck`
  rejects a cap-carrying `Option`/`Result`. (This is the same shape as §7's
  `List(Net)` reject; revisit together if a program needs it, by representing the
  payload as a GC field rather than a Slot.)
- **Closure environments — REPRESENT (no crossing).** Cap-carrying closures are
  already GC structs (§4.2): the captures are `externref` struct fields, not
  Slots, so there is no crossing to reject.
- **Grantable-cap fields — REPRESENT (mandatory).** The RFC-0038 mint path today
  widens each grantable-cap field to the i64 slot via `build_user_cap_field` +
  `Convert{I32→I64}` before `mk{N}`
  (`crates/witchy-lower/src/codegen/assembly.rs:721-731`). Under externref that
  `Convert` is impossible for a cap field; the field becomes an `externref` GC
  struct field minted directly (scalar fields keep their Slot widening). This one
  crossing is *replaced by representation*, not rejected — the feature ships, so
  it cannot simply be forbidden.

**Error-text style** mirrors §7 and the existing attenuation errors (lowercase,
name the offending type, give the remedy): e.g. `a capability cannot flow through
a generic parameter (it has no boxed representation); give the parameter a
concrete capability type` and `a capability cannot be wrapped in \`Option\`/\`Result\`
in the first cut; pass it directly`. Cap-carrying aggregates get a distinct
message: they require RFC-0005's GC-struct aggregate lowering and are rejected
until that representation exists.

## 5. Instantiation: minting the root capabilities

**Correction to the naive picture.** Today the `run` wrapper takes *no*
capability arguments: it bakes the handles *internally* as `ConstI32` — `Dir`
params get `0,1,2,…`, `File` params their own index, every other cap a
placeholder `0` — and grantable-cap params are minted guest-side by `mk{N}` over
`build_user_cap_field` (`crates/witchy-lower/src/codegen/assembly.rs:698-731`).
So the current export signatures carry no authority in their types.

An `externref` cannot be a `ConstI32`, but the wrapper can keep the public ABI
stable by calling a host-only mint import. Stage 2 uses that shape: the generated
no-arg `run` wrapper calls `mint_file(i)` for each direct `--file` grant ordinal,
receives a `File` externref, and passes it to `main`. The grant ordinal is still
an integer, but it never becomes the guest's `File` value.

Exported entrypoints are intentionally not generalized in Stage 2. RFC-0040's
cap-gated `__export_*` path currently mints guest records for user-defined
grantable capabilities; a migrated host capability such as `File` is not a valid
leading export capability until the export ABI has a real minting story for it.
This keeps the browser/glamour ABI stable for pure string exports while avoiding
a half-i32-half-externref export surface.

If a later stage chooses to pass root capabilities as export parameters instead
of minting inside wrappers, it reaches beyond the runtime:

- **`Runtime::spawn`** supplies the externrefs at the wasmtime call site (it
  already builds the grant set; it now wraps each grant in an `externref` and
  passes it positionally).
- **The browser / JS shim** (RFC-0006–0008) that invokes `run()` / `__export_*()`
  must pass the minted references; the marshaling contract for those exports
  changes. A pure-compute frontend that grants no caps is unaffected (no
  externref params), but any cap-holding frontend build is (ties to the browser
  risk, §8.2).
- **glamour** (RFC-0040) binds the `__export_*` cap-gated exports; its export
  glue moves from "wrapper mints the grantable cap" to "host passes the cap
  externref in." That binding is regenerated in lockstep with the ABI cut.

Migration rule: prefer wrapper-local minting when the wrapper is generated by
Witchy (`run`, and any future host-owned wrapper), because it keeps public exports
stable. Only change export signatures when the host genuinely must provide a live
reference value that the wrapper cannot mint from existing grant state.

## 6. Staging (how an "all-or-nothing" cut is still reviewable)

The ABI can't be half-i32-half-externref *for one capability*, but it **can** be
migrated one capability TYPE at a time if the classification and the boundary are
done first. Proposed order, each stage its own PR, `check.sh` green at each:

1. **Infra, no behavior change.** Add the `WirTy::Extern`/`GcRef` variants, the
   type-section encoder, and the `StructNew/Get/Set` nodes — all unused. Add
   `carries_cap`. Prove the encoder round-trips a hand-built GC-struct module
   (a `wir_encode_tests` fixture). Nothing lowers to them yet. GREEN.
2. **`File` end to end (the proving capability).** Mint `File` as an `externref`;
   `host_file_read_len`/`host_file_write` take `externref` and downcast; the
   no-arg `run` wrapper calls `mint_file(i)` and passes the resulting externref
   to `main` instead of passing the old `ConstI32` file index as the File value.
   **Why File, not Console or Exec:** Console/Clock are excluded (§3 — no handle
   to migrate). Of the handle-bearing caps, File is the right first proof because
   it is *rights-bearing* (`File[Read]`/`File[Write]`), so it exercises the
   interesting path — a downcast that must still honor the type-checked rights —
   not just a bare handle; its grant is the simplest object (a `PathBuf` index in
   `VmState.files`); and it already has the anchoring attenuation slice
   (`file_capability_rights_and_narrowing`) to hold parity against. Exec would
   prove less (right-less, one op) at higher blast radius. GREEN (all File
   programs, both backends).
3. **Aggregate/API reconciliation before the next root-cap widening.** Do not
   add `Dir`, `Net`, or `Secret` to `is_externref_cap` as a mechanical next step.
   Unlike `File`, they are not isolated leaf values in shipped source:
   `capability ConfigDir from Dir`, `capability Redis from Net`, and
   `Option(Dir)` are valid today, while `std/secretstore.get -> Option(Secret)`
   depends on `Secret` crossing the slot boundary. Widening the seam without a
   representation/API answer would make those language features fail, not make
   RFC-0005 coherent. Land one of these decisions first:
   - **Represent** cap-carrying records/tuples/branded caps/UI tokens as GC
     structs, then closures capturing caps; and represent or redesign the
     `Option`/`Result` payload path for `Secret`; or
   - **Reject deliberately** by changing the public surface, tests, and docs in
     one reviewed language cut.
   The preferred 0.1 route is representation for branded/user capability
   wrappers; deleting shipped branded-cap examples to unblock a migration is not
   an acceptable silent simplification.
4. **The remaining guest-represented root + derived handles** (`Dir`, `Net`,
   `Secret`, and the `Net`-derived `Socket`/`Listener`) — each as an `externref`
   param or `externref`-returning import (`connect`/`listen`/`accept` return an
   `externref` Socket/Listener instead of an i32 index). Imports downcast. GREEN
   after each. Zero-representation caps (`Console`/`Clock`/`Rand`/`Env`/`Exec`/
   `SecretStore`) are not here because there is no guest handle to migrate.
5. **Delete the i32 handle machinery** — `VmState.dirs/nets/files/sockets/
   listeners/secrets`, the index arithmetic, the `*_handle` conventions, and the
   temporary two-mode mint. The suite staying green with them gone is the proof
   the migration is total. (The host keeps canonical authority sources or
   identity-keyed roots where needed — see §8.9; "delete the tables" means the
   guest-facing index tables, not host-owned authority.)

Each stage is independently committable and green because a capability type not
yet migrated keeps its i32 path until its stage — the "cannot coexist" constraint
is *per capability type*, not global, once the boundary supports both minting
paths during the transition (a temporary two-mode mint, deleted in stage 5).

## 7. Cap-carrying collections (`List(Net)`, `Dict(String, Dir)`)

Resolved by representation:

- **`List(T)` is supported** when `T` is reference-bearing. Lowering allocates a
  typed GC array whose element is the exact `externref` or concrete `GcRef`
  kind. Literals, length, indexed reads, iteration, and persistent
  push/set/concat preserve that kind.
- **`Dict(K, V)` remains rejected** when either stored side is
  reference-bearing. Its hash-table cells still use universal i64 slots; support
  requires a typed table layout rather than an exception in one operation.

## 8. Open questions / risks (the review agenda)

1. **GC × rc-floor.** The linear-memory reclamation (RFC-0016, `$rc_alloc`/
   `$rc_free`/`$rc_dup`) must not touch GC refs and vice-versa. Since cap-carrying
   shapes are GC and everything else is linear, the two heaps are disjoint by
   construction — but a cap-carrying record with *scalar* fields now holds those
   scalars in GC fields, so any codegen that assumed "record fields are linear
   words" must be audited. This is the main correctness risk.
2. **Browser / frontend target.** The Glamour frontend compiles witchy to
   wasm for the browser (RFC-0006–0008). wasm-gc ships in current browsers, but
   the pure-compute shim must be re-checked. If a frontend app ever holds a
   cap-carrying value (the UI tokens are `capability` types!), those become GC
   structs in the browser build — verify the shim + `glamour-dom.mjs` path. This
   may gate the cut on a browser-gc baseline.
3. **`frozen`/`unique` on cap-carrying values.** The ownership qualifiers and the
   escape analysis assume linear-memory values; confirm they compose with GC
   structs (they should — the analysis is type-directed, not representation-
   directed — but it needs a test pass).
4. **Debuggability.** GC struct traps give worse backtraces than linear-memory
   OOB; keep the `WITCHY_WASM_BACKTRACE` name section working for GC funcs.
5. **Wasmtime API churn.** `ExternRef`/GC API is newer; pin the version and adapt
   (per the repo's "latest libs" rule).
6. **Socket/Listener — DECIDED: scope in.** The `Socket` and `Listener` values
   returned by `connect`/`listen`/`accept` are guest-indexed i32 handles into
   `VmState.sockets`/`listeners` (`crates/witchy-runtime/src/runtime.rs:280-283`)
   — the same forgeable-authority-in-linear-memory shape this cut exists to kill.
   A corrupted in-range socket index cannot escape the `Net` scope (the address
   was checked at connect time), but it *can* cross the data of two live
   connections the program holds — write a secret to the wrong socket, read the
   wrong response. Under witchy's threat model a live authenticated connection is
   authority, and "no forgeable authority in linear memory" must include it;
   leaving Socket/Listener as i32 would be an inconsistent residual. They migrate
   in stage 3 as `externref`-*returning* imports (minted mid-execution when the
   connection opens, not at instantiation) — a natural fit since the host already
   owns the `BufReader`/`TcpListener`.
7. **Equality / render / reflect over cap-carrying records — all REJECT.** A
   capability is an opaque unforgeable reference with *identity*, not data, and an
   `externref` has no bytes to compare, print, or serialize. `typeck` refuses
   these on any cap-carrying type, first cut:
   - **`==` (RFC-0047).** A cap-carrying type does not satisfy the `Eq` bound;
     `==` on it is a type error. (Structural equality would have to compare
     `externref`s, which have only reference identity — not the value equality
     `==` promises.) Rejecting keeps the one-equality story honest.
   - **`show` / `__render` / `${…}`.** A cap-carrying value has no `Show`;
     rendering it is a type error. Beyond representation, rendering a grant risks
     leaking it — opaqueness is the right default.
   - **`derive(Reflect)` / `json.stringify`.** `derive(Reflect)` is refused on a
     record with a cap-carrying field (it would either leak the grant or demand a
     placeholder), so `json.stringify` over such a value is unreachable by
     construction. Error style per §4.4/§7.
   Each is defense-in-depth *and* a hard representational fact under externref;
   the reject makes the compiler say so instead of a backend silently diverging.
8. **Caps crossing `chan.spawn` / VM boundaries — externrefs can't cross Stores.**
   A wasmtime `externref` is rooted to one `Store`; it cannot be handed to
   another VM's Store. Today the guest never transports a capability across that
   boundary: a `serve`-pool worker VM (RFC-0032) receives its authority because
   the host *re-derives* the worker's grant set from the shared `Capabilities`
   (`vmstate_from_caps`, `crates/witchy-runtime/src/runtime.rs`), minting fresh
   handles into the worker's own Store — not by the guest passing a handle over a
   channel; and cooperative `std/chan` tasks share the parent VM's single Store,
   so a cap captured by a `Task` closure never leaves it. What `typeck` does *not*
   yet enforce is a rule forbidding a cap-carrying **channel message type** — it
   was moot while the common `spawn` is cooperative and message handles would be
   meaningless across Stores anyway. The cut makes this a hard requirement: add a
   `typeck` reject for a cap-carrying type used as a channel message type (or
   otherwise crossing a Store), landed with stage 3/4. The worker path is
   unchanged — the host keeps re-minting per Store.
9. **Wasmtime GC rooting — delete guest-facing tables, not host authority
   ownership.** A `Rooted<ExternRef>` is scoped to a `Store`, and the guest-held
   reference keeps its host data alive while it is reachable from wasm. Stage 2
   therefore stores the confined `PathBuf` directly in each minted `File`
   externref; no dense `File` index table survives in guest-observable form. The
   host must still own the canonical authority *source* used to mint caps: direct
   grants stay in `Capabilities`/`VmState` long enough for wrapper-local minting,
   and any future host-retained or cross-Store capability must be re-minted or
   held through an identity-keyed host root/`Arc<Grant>`, never through the old
   integer table. Stage 5's "delete the i32 tables" means deleting only the
   guest-facing forgeable representation, not deleting the host's ability to
   prove and recreate authority.

## 9. Definition of done (unchanged from RFC-0005)

All capability imports take `externref`; the i32 handle tables are deleted (the
host still owns or can re-mint the underlying authority, §8.9); parity is green
on both backends; the differential fuzzer (augmented with adversarial aliasing over
cap-carrying aggregates) finds no diffs; and the attenuation suite (RFC-0005
hardening #4) stands as the runtime backstop. **That suite was File-only when
this plan was first drafted** — the "already comprehensive" claim here was wrong.
It was extended to Net, Dir, and `only`-policy rights/narrowing/re-widening by
**BUG-009** (landed 2026-07-04, `crates/witchy-types/src/typeck_tests.rs`), which
was the hard prerequisite for beginning the cut. With BUG-009 in, the Stage-1
gate is clear. At that point a miscompile is a *correctness* bug (wrong data, a
trap), never a *security* bug — the thesis of RFC-0005.

---

<!--
  This is a living companion doc (status: planned), not a frozen decision. It
  refines the implementation of RFC-0005's already-chosen approach (A). Edit
  freely until the cut begins; then it tracks the actual migration.
-->

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9) — this review served as the pending design review.

**Status-accuracy corrections / gaps.** The staging strategy — per-cap-type
migration, two-mode mint, green at each stage — is right. Four substantive gaps
block approval:
1. **Stage 2 (Console) has a wrong premise:** print/Clock have NO runtime handle
   today — `host_print` takes ptr/len and codegen drops the Console arg
   (builtins.rs:383-391); zero-representation caps are already unforgeable.
   Rescope to the handle-bearing caps (Dir/Net/File/Secret/Exec) and prove the
   mechanism on Exec or File.
2. **The i64 Slot boundary is unaddressed:** externref cannot round-trip through
   ToSlot/FromSlot (generic calls, closure envs, Option payloads, grantable-cap
   fields widened to slots at assembly.rs:721-731). Needs an explicit
   reject-or-represent rule.
3. Socket/Listener migration is undecided.
4. **§5 misdescribes minting:** the run wrapper embeds ConstI32 handles
   internally, so externref changes the `run`/`__export_*` signatures — reaching
   the browser shim and glamour (RFC-0040) — which the plan does not acknowledge.

Also to enumerate: equality/render/reflect over GC structs; caps crossing
`chan.spawn` (externrefs can't cross Stores); wasmtime `Rooted<ExternRef>`
lifecycle means the host must separate guest-facing handles from host-owned
authority sources after "delete the tables." The claim that the attenuation suite
is "already comprehensive" is false — coverage is File-only
(typeck_tests.rs:215-233; BUG-009).

**Filename.** The former name `0005-externref-implementation-plan.md` violated
the numbering rules (rfcs/README.md:51-54: numbers allocated in order,
single-decision docs only); renamed to this un-numbered companion form, matching
the `identity-stack-implementation-plan.md` precedent. Cross-links updated.

**Verdict.** Needs-revision before approval. Priority: medium — the revision is
cheap, but the cut itself is Size-L and queues behind the language-surface work.
Do not begin Stage 1 until the revision lands and the Net/Dir attenuation suite
(BUG-009) exists.

## Revision applied (2026-07-04)

The four blocking gaps and the enumeration items above are now resolved in the
body; `status: design → planned`.

- **Stage rescope (§3, §6).** `Console`/`Clock`/`Rand`/`Env`/`Exec`/
  `SecretStore` dropped from `carries_cap` and from the migration — they are
  zero-representation (no guest-held runtime handle), already unforgeable. Stage
  2's proving capability is now **File** (rights-bearing, simplest grant, has
  the anchoring attenuation slice); rationale recorded inline. Stage 3 is no
  longer a blind "remaining root caps" sweep: aggregate/API reconciliation must
  precede migrating `Dir`/`Net`/`Secret`.
- **i64 Slot boundary (§4.4, new).** Reject-first, symmetric with §7: generic
  Slot-boxing and `Option`/`Result` payloads are `typeck`-rejected; closure envs
  and grantable-cap fields are *represented* (GC fields) rather than crossing;
  error-text style specified.
- **Socket/Listener (§8.6).** DECIDED — scoped in (stage 3, `externref`-returning
  imports); rationale per threat model (a live connection is authority).
- **§5 minting surface.** Rewritten after Stage 2: wrapper-local minting keeps
  `run()` stable (`mint_file(i)` inside the generated wrapper). `__export_*`
  remains deliberately unexpanded for migrated host caps until a real export ABI
  story exists.
- **§8 additions.** Equality (RFC-0047) / `show`·`__render` / `derive(Reflect)`·
  `json.stringify` over cap-carrying records — all reject, with rationale;
  caps-across-`chan.spawn` rule (host re-mints per Store; `typeck` reject for
  cap-carrying message types to be added); wasmtime `Rooted<ExternRef>` lifecycle
  clarified as "guest tables go away, host authority ownership stays."
- **§9 DoD corrected.** The "already comprehensive" attenuation claim was false
  (File-only at drafting); **BUG-009 landed the Net/Dir/policy coverage
  (2026-07-04)**, clearing the Stage-1 prerequisite.

**Post-revision verdict.** Approvable. Stage 1 (infra, no behavior change) may
begin now that BUG-009 has landed; the cut still queues behind the
language-surface work by priority, not by any missing prerequisite.

## Implementation status (2026-07-05)

`status: planned → in-progress`. Stage 1 (infra) and the §4.4 Slot-boundary reject
have LANDED, green on both backends (728 differential tests + the crate suites).
Stage 2 (File end-to-end) is scoped and de-risked but NOT started — it is a single
coordinated ABI cut with no safe partial (see below).

**Landed (branch `rfc-0005-externref-stage1`):**
- **WIR infra (§4.1), no behavior change.** `Kind::ExternRef` / `Kind::GcRef(u32)`
  and `WirTy::Extern` / `WirTy::GcRef`; `WirStructDef` + `StructNew`/`StructGet`
  (WirExpr), `StructSet` (WirNode), `RefNull(Kind)`; a `wir_encode` type section
  that emits GC struct defs and wires `struct.new`/`get`/`set` + `ref.null`. The
  encoder lays struct types right after the reserved `$clos{N}` band (indices
  `0..=MAX_CLOS`) and before the other function signatures, shifting non-clos sig
  indices up by `structs.len()` — GC recursion-group scoping forbids a *forward*
  reference across singleton type defs, so a `GcRef`-param function must follow
  its struct. `encode` gained a `structs: &[WirStructDef]` argument; every current
  caller passes `&[]`, so struct-free modules (the whole production path) encode
  byte-identically. A round-trip test builds a `{externref, i64}` struct + a
  function carrying both `externref` and `(ref null $0)` params + all four opcodes
  and validates/executes it in wasmtime (GC + function-references enabled).
- **`carries_cap` classification (§3) + i64 Slot-boundary reject (§4.4/§7).**
  `carries_externref_cap` (typeck) resolves whether a type transitively holds a
  migrated-to-externref capability, recursing through user `type`/`capability`
  declarations with a cycle guard. `reject_cap_slot_boundary` refuses a migrated
  capability wrapped in `Option`/`Result`/`List`/`Dict` (the slot-boxed forms) and
  cap-carrying tuples/user records until the GC-struct aggregate stage exists. A
  bare capability param/return is allowed (stays an `externref`). The migrated set
  is a single seam, `is_externref_cap` — currently `{File}` — that widens per stage;
  sibling caps on the i32 path (e.g. `std/secretstore.get -> Option(Secret)`) still
  type-check.

**In progress — Stage 2 (File end-to-end) and why it is one atomic cut.** The
File ABI cannot be half-i32-half-externref: a File value originates from a direct
`--file` `main` param AND from `dir.open`/`dir.create`, and is consumed by
`file_read`/`file_write` — all naming the same representation. Migrating File
therefore means, in lockstep: (a) `file_read`/`file_write` host imports + their
WIR helpers take an `externref` and downcast to the backing `PathBuf` grant; (b)
`dir_open`/`dir_create` return an `externref`; (c) the WIR lowering treats bare
`File` params/locals/results as `WirTy::Extern`; (d) the generated no-arg `run`
wrapper keeps its public signature stable and calls the host-only `mint_file`
import to turn each direct `--file` grant index into a `File` externref before
calling `main`. Wasmtime's nullable core `externref` maps to
`Option<Rooted<ExternRef>>` at host boundaries; Witchy mints only `Some(...)` and
host file ops reject `None` loudly. Dir/Net/Secret/Exec/Socket/Listener stay on
the i32 path until their own stages.

Branch `impl/rfc-0005-stage2` now has the first end-to-end File cut following
that design: direct `main(File[Read])` grant-doc execution and the
`file_capability` Dir-navigation example both pass on the compiled backend, and
`witchy sandbox --file` has explicit read/write coverage. Typeck rejects
cap-carrying tuples/user records and cap-carrying closure captures until the
GC-struct aggregate stage exists; the same guard now covers inferred aggregate
construction (`[f]`, `(f, 1)`, `dict.insert(..., f)`, generic records such as
`Box(f)`), not only annotated signatures. Typeck also rejects `export_*`
entrypoints that try to expose `File` as a leading cap. A hostile precompiled
module that passes `ref.null extern` to `file_read_len` traps with
`File externref is null`.

**Validation checkpoint (2026-07-06, final integration branch
`integrate/rfc0005-final`).** The File externref cut has been checked with
targeted coverage for direct `--file` read/write grants, `file_capability` Dir
navigation, `file_capability_rights_and_narrowing`, and the hostile
null-externref runtime fixture (`cargo test -p witchy-runtime --features native
null_file_externref_is_rejected`). The branch is based on BUG-550's compiled-wasm
package-manager front end and carries the glamour chart fixture needed by the
current full gate. Before BUG-550, the glamour e2e timed out as an
interpreter-backed package-manager path; with BUG-550, it passes in isolation in
10.575s on `master` and 11.123s on this integration stack. The final
`./scripts/check.sh` gate for `integrate/rfc0005-final` is the merge authority.

## Implementation status (2026-07-13)

Stage 2 and the guest-represented Stage 3 surface have landed on `master`:
`File`, `Dir`, `Net`, the derived `Socket`/`Listener` values, and `Secret` now
cross the compiled host boundary as opaque `externref`s. `SecretStore` and
`Exec` are zero-representation authority surfaces: the source value is checked,
but the host import/link grant carries the runtime authority rather than a guest
handle. The same is now true for build capabilities (`BuildOut`, `BuildRead`,
`BuildEnv`, `BuildNet`, `BuildExec`): their host ABI no longer accepts an
ignored leading `i32` receiver.

Stage 5 is therefore narrowed from "delete all `VmState.dirs/nets/files`"
to the actual invariant: delete every **guest-facing** integer authority handle.
The runtime still keeps root grant material (`Dir`/`File`/`Net` grants and
build grants) so generated wrappers can mint externrefs from grant ordinals, but
those ordinals never become guest capability values. The remaining cleanup is
terminology/API polish (`*_handle` names that mean host authority objects) and
the deferred Stage 4 representation work for cap-carrying aggregates and
closures.

## Stage 4 progress (2026-07-13)

**Nominal aggregate slices landed.** The GC-struct representation proved by
sealed `capability` records now covers every non-generic nominal aggregate:
named-field records, positional wrappers, and multi-variant sums. The
representation-neutral `ReferenceStorageClassifier` is the semantic home and
`gc_cap_aggregate_names` adds the current non-generic lowering boundary; typeck
and codegen consume that same set. This closes BUG-566 (typeck's recursive
classification vs codegen's direct-field copy disagreed on nested records →
checked-valid programs ICE'd the encoder). The records slice also added the GC spread path
(`T(field: v, ..base)` → `StructNew` over `StructGet`s, which place assignment
desugars to) and excluded ref-typed records from SROA and the RFC-0033
in-place record update (both slot-box fields; a GC record binds as one
`GcRef` local and rebuilds by `StructNew`).

The sum slice uses one struct layout per nominal type: an `i32` tag followed by
disjoint per-variant field bands. Constructors zero inactive scalar fields and
write null to inactive reference fields. Pattern lowering tests the tag before
projecting the active band, including nested recursive references. All GC IDs
are reserved before field kinds are materialized, so self-recursive and
mutually-recursive sums share the encoder's one explicit recursion group.

**Concrete structural tuple slice implemented.** A direct, fully concrete tuple
that transitively carries an externref capability is interned as a typed GC
struct by a deterministic recursive field-shape key. Nominal IDs retain module
declaration order; tuple IDs append in sorted shape order, and every ID is
reserved before nominal or tuple fields are materialized. Construction,
numeric projection, direct parameter/result ABI, qualifiers, `let` and `match`
patterns, nested tuples, and tuples nested in nominal records/sums use the same
reference-typed path on the compiled backend. The interpreter remains the
semantic oracle and differential tests cover all of those shapes plus stable
binary output.

The boundary remains fail-closed where no typed representation exists:
capability tuples cannot instantiate the scalar generic ABI, enter
`List`/`Dict`/`Result` storage, mix with a first-class function value, escape a
`region:`, render, or compare for equality. Type aliases are expanded before
type checking and representation selection, so concrete and generic aliases of
a supported tuple use this same structural path. Closures remain the hard tail
(a capture is invisible in the function TYPE, so cap-carrying and scalar
closures flowing into one `fn`-typed param force a uniform environment
representation).

## Stage 4 closure continuation (2026-07-15)

The closure tail is proceeding as separately green cuts rather than one ABI
rewrite. First, the fail-closed capture predicate was made total over all six
migrated authority values (`Dir`, `File`, `Net`, `Socket`, `Listener`, and
`Secret`), their transparent brands, and nested GC records. This closes the path
where several capability kinds passed checking and reached an impossible
`externref -> i64` encoder conversion.

The next landed substrate defines one future closure-wrapper layout:

```text
Closure {
    code_index: i32,
    linear_env: i32,
    gc_env: structref,
}
```

`structref` is an erased nullable GC-struct reference. WIR represents that type
and a checked `ref.cast` back to a concrete `GcRef`; a Wasmtime validation test
constructs the wrapper, erases a payload containing an `externref`, casts it
back, and reads it. Production source lowering now uses this wrapper for every
function value and emits per-lambda typed payloads before removing the capture
rejection. A capability never enters `linear_env` or an i64 slot.

The following substrate cut replaces WIR's lossy indirect-call key
`(source arity, result count)` with an exact wasm parameter/result signature.
The existing scalar convention remains available as a constructor and retains
its stable reserved type band, while typed signatures can name `externref`,
`structref`, and concrete GC structs after those structs are declared. The
encoder checks argument and destination counts against the signature, and the
proper-tail-call dispatcher stages each operand in a local of its exact kind.
Tests place same-arity scalar- and GC-environment functions in one table and
validate a GC-environment indirect tail cycle. That substrate checkpoint was
source-neutral; the following cut began selecting exact signatures while the
then-current scalar closure environment remained unchanged.

The first source-enabling signature cut now selects that exact signature for a
function value whenever a parameter, declared result, or `var` write-back is an
`externref` or concrete `GcRef`. Lifted bodies, direct and devirtualized calls,
`call_indirect`, and explicit returns preserve those kinds. Checker-resolved
signatures drive inferred lambdas, so an unannotated `fn(x): x` applied to a
capability cannot fall back to scalar lowering. Scalar-only function values keep
the established i64-slot ABI.

Named polymorphic functions now use first-class monomorphization: the checker-resolved
function type specializes the referenced body before forwarding-closure
construction, including capability rights, concrete GC aggregates, bounds, and
`var` write-backs. This includes result-only type variables, function references
returned through generic wrappers, and unannotated non-generic parameters;
specialization runs to convergence and rejects non-convergence loudly. Direct
generic calls remain supported. Isolated worker
callback adapters retain an explicit cross-instance typed-signature gate.

The next source-neutral cut adds mutable typed GC arrays to WIR. Structs and
arrays share one concrete GC type-index band and recursion group before
reference-bearing function signatures, so forward and cyclic aggregate edges
remain valid. Array operations cover repeated and fixed allocation, indexed
read/write, and length. An executable Wasmtime fixture stores GC payload structs
containing `externref` in an array, follows a forward struct-to-array edge, passes
the concrete array reference in a function signature, runs the optimizer over
every operation, and validates the result. The existing
`encode(module, structs)` entrypoint still emits no arrays. Production uses the
array-enabled encoder for every demanded closed `List(T)` whose element is a
reference. Closed generic nominal instances are keyed by complete semantic type
identity and materialized after all recursive IDs are reserved. Nullable
`Option(reference)` and typed `Result` sums complete the closed container
matrix. `Dict`, open generic function ABIs, region copy-out, and isolated-worker
crossings remain reject-first.

## Stage 4 closed-layout completion (2026-07-17)

The final Stage 4 slice replaced declaration-keyed GC aggregate registration
with demand-planned closed layouts. The plan is seeded from function signatures
and concrete checker facts, instantiates implicit or explicit type parameters,
and recursively discovers nominal, tuple, and list dependencies. Canonical
keys include linked declaration identity plus every concrete type argument, so
two instances cannot accidentally share a Wasm field layout.

Executable parity covers direct and nested closures, `Box(fn)`, `Box(File)`,
recursive `Chain(fn)`, `Option(fn)`, `Result(fn, String)`,
`List(Box(fn))`, and `List(File)` under optimized, boxed, indirect, and
optimization-disabled configurations. The standard `dedup` and async-task
programs exercise the linked implicit-generic `Iter`/`Step` and `Task` graphs.
The encoder remains the backstop: no `ExternRef`, `StructRef`, or `GcRef` may
cross `ToSlot`/`FromSlot`.
