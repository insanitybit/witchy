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
> and — where possible — independently green. **No code ships from this document;
> it exists for review before the ABI cut begins.**
>
> Written design-first at the maintainer's request: the externref core is a
> Size-L, all-or-nothing ABI cut that "cannot coexist with the i32 ABI", so it is
> deliberately kept out of the autonomous loop until this plan is approved.

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
*handle-bearing* capability:

- a **handle-bearing** capability type — `Dir`, `Net`, `File`, `Secret`,
  `SecretStore`, `Exec`, the derived `Socket`/`Listener`, and every
  `capability`/`grantable capability` declaration (incl. the RFC-0002 branded
  forms like `Redis(net)` and the RFC-0039 UI tokens);
- a record/`type` variant with any cap-carrying field;
- a tuple with any cap-carrying element;
- a closure whose captured environment has any cap-carrying capture;
- `Option`/`Result`/`List`/`Dict` **only** if their element/payload is
  cap-carrying (a `List(Int)` is not; a `List(Net)` is — rare but must be handled
  or explicitly rejected, see §7).

**`Console` and `Clock` are excluded — they are zero-representation
capabilities.** Neither has a runtime handle: `host_print` takes only `ptr/len`
and codegen *drops* the `Console` argument entirely
(`crates/witchy-lower/src/codegen/builtins.rs:383-391`), and `Clock` is the same
— it names a host op, not a stored grant. A capability with no bytes in linear
memory is already unforgeable: there is nothing to corrupt, swap, or mint. So
`carries_cap` is false for them and they are absent from the migration (§6); the
cut only touches capabilities that name a host-side grant.

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
- **Closures capturing a cap:** the closure environment for a cap-carrying
  closure becomes a GC struct (captures as struct fields), and the code index
  stays a `funcref`. A closure with only scalar captures is unchanged (linear
  memory). The `CallIndirect` dispatch is unaffected (funcref table); only the
  environment pointer's type changes from `i32` to a `GcRef`.

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
in the first cut; hold it in a record field or pass it directly`.

## 5. Instantiation: minting the root capabilities (an export-surface change)

**Correction to the naive picture.** Today the `run` wrapper takes *no*
capability arguments: it bakes the handles *internally* as `ConstI32` — `Dir`
params get `0,1,2,…`, `File` params their own index, every other cap a
placeholder `0` — and grantable-cap params are minted guest-side by `mk{N}` over
`build_user_cap_field` (`crates/witchy-lower/src/codegen/assembly.rs:698-731`).
So the current export signatures carry no authority in their types.

An `externref` cannot be a `ConstI32`. The host must therefore **mint the
externrefs and pass them as new parameters** to the wrappers — which **changes the
export signatures**: `run` gains one `externref` param per granted root cap (in
declaration order, where the `ConstI32`s used to sit), and each cap-gated
`__export_*` wrapper (RFC-0040) that currently mints its grantable cap in-body
takes that cap as an `externref` param minted host-side instead. This is the piece
the earlier draft missed, and it reaches beyond the runtime:

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

Migration step: land the signature change behind the stage-3/4 two-mode mint (the
`ConstI32` path stays for not-yet-migrated cap types); the browser shim and
glamour export bindings update in the same stage that flips their capability to
externref. The `--dir`/`--net`/`--secret`/`--signing-key` grant plumbing
(`main.rs`) is unchanged above the mint point; only the handle-vs-externref
hand-off — now a *parameter*, not an embedded constant — moves.

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
   `run` wrapper passes it as a param instead of the `ConstI32` file index.
   **Why File, not Console or Exec:** Console/Clock are excluded (§3 — no handle
   to migrate). Of the handle-bearing caps, File is the right first proof because
   it is *rights-bearing* (`File[Read]`/`File[Write]`), so it exercises the
   interesting path — a downcast that must still honor the type-checked rights —
   not just a bare handle; its grant is the simplest object (a `PathBuf` index in
   `VmState.files`); and it already has the anchoring attenuation slice
   (`file_capability_rights_and_narrowing`) to hold parity against. Exec would
   prove less (right-less, one op) at higher blast radius. GREEN (all File
   programs, both backends).
3. **The remaining root + derived caps** (`Dir`, `Net`, `Secret`, `SecretStore`,
   `Exec`, and the `Net`-derived `Socket`/`Listener`) — each as an `externref`
   param or `externref`-returning import (`connect`/`listen`/`accept` return an
   `externref` Socket/Listener instead of an i32 index). Imports downcast. Still
   no aggregates. GREEN after each. (`Console`/`Clock` are not here — nothing to
   migrate.)
4. **Cap-carrying aggregates** — records/tuples/branded caps/UI tokens to GC
   structs; then **closures** capturing caps. This is the hard stage; the
   `carries_cap` classification, the GC-struct lowering, and the §4.4 Slot
   rejects land here. GREEN.
5. **Delete the i32 handle machinery** — `VmState.dirs/nets/files/sockets/
   listeners/secrets`, the index arithmetic, the `*_handle` conventions, and the
   temporary two-mode mint. The suite staying green with them gone is the proof
   the migration is total. (The host keeps its *ownership anchors* for the minted
   references — see §8.9; "delete the tables" means the guest-facing index
   tables, not the host's rooting set.)

Each stage is independently committable and green because a capability type not
yet migrated keeps its i32 path until its stage — the "cannot coexist" constraint
is *per capability type*, not global, once the boundary supports both minting
paths during the transition (a temporary two-mode mint, deleted in stage 5).

## 7. Cap-carrying collections (`List(Net)`, `Dict(String, Dir)`)

Rare but real. Two acceptable resolutions, decide at stage 4:
- **Support:** a `List`/`Dict` whose elements are cap-carrying becomes a GC
  `array`/struct-of-arrays. More lowering surface.
- **Reject (recommended first cut):** `typeck` refuses a collection literal /
  type whose element is cap-carrying, with a clear error ("a capability cannot be
  stored in a `List`; hold it in a record or pass it directly"). This matches how
  capabilities are meant to flow (named, not bulk-stored) and defers the GC-array
  lowering. Revisit only if a real program needs it.

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
9. **Wasmtime GC rooting — the host keeps an ownership anchor after "delete the
   tables."** A `Rooted<ExternRef>` is scoped to a `RootScope`/`Store` and may be
   collected once its scope ends. For a granted capability to stay live for the
   instance's lifetime, the host must hold a **`ManuallyRooted<ExternRef>`** (or
   keep the backing `Arc<Grant>` owned host-side) per minted cap — otherwise the
   guest's reference and its backing grant could be GC'd out from under a live
   program. This anchor is **identity-keyed** (keyed by the reference / grant
   identity), *not* the old dense index. So stage 5's "delete the i32 tables"
   deletes only the **guest-facing index tables**; the host's rooting/ownership
   set stays. Getting this wrong dangles a grant, so it is called out as its own
   line item, not folded into §8.1.

## 9. Definition of done (unchanged from RFC-0005)

All capability imports take `externref`; the i32 handle tables are deleted (the
host keeps its identity-keyed rooting anchors, §8.9); parity is green on both
backends; the differential fuzzer (augmented with adversarial aliasing over
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
lifecycle means the host still needs an ownership anchor after "delete the
tables." The claim that the attenuation suite is "already comprehensive" is
false — coverage is File-only (typeck_tests.rs:215-233; BUG-009).

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

- **Stage rescope (§3, §6).** `Console`/`Clock` dropped from `carries_cap` and
  from the migration — they are zero-representation (no runtime handle;
  `builtins.rs:383-391`), already unforgeable. Stage 2's proving capability is
  now **File** (rights-bearing, simplest grant, has the anchoring attenuation
  slice); rationale recorded inline. Stage 3 is the remaining root + derived caps.
- **i64 Slot boundary (§4.4, new).** Reject-first, symmetric with §7: generic
  Slot-boxing and `Option`/`Result` payloads are `typeck`-rejected; closure envs
  and grantable-cap fields are *represented* (GC fields) rather than crossing;
  error-text style specified.
- **Socket/Listener (§8.6).** DECIDED — scoped in (stage 3, `externref`-returning
  imports); rationale per threat model (a live connection is authority).
- **§5 export-surface change.** Rewritten: the `run`/`__export_*` signatures gain
  externref params (today they bake `ConstI32` handles + `mk{N}`), reaching the
  browser shim and glamour (RFC-0040); migration step named.
- **§8 additions.** Equality (RFC-0047) / `show`·`__render` / `derive(Reflect)`·
  `json.stringify` over cap-carrying records — all reject, with rationale;
  caps-across-`chan.spawn` rule (host re-mints per Store; `typeck` reject for
  cap-carrying message types to be added); wasmtime `Rooted<ExternRef>` anchor
  (host keeps identity-keyed roots after the index tables are deleted).
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
  capability wrapped in `Option`/`Result`/`List`/`Dict` (the slot-boxed forms),
  wired into `check_type_names` over params, returns, and record fields. A bare
  capability param/return is allowed (stays an `externref`); a capability in a
  record/tuple field is allowed (the GC-struct aggregate path, §4.2). The migrated
  set is a single seam, `is_externref_cap` — currently `{File}` — that widens per
  stage; sibling caps on the i32 path (e.g. `std/secretstore.get -> Option(Secret)`)
  still type-check.

**Not started — Stage 2 (File end-to-end) and why it is one atomic cut.** The
File ABI cannot be half-i32-half-externref: a File value originates from a `--file`
`main` param (baked as `ConstI32` in the run wrapper, `assembly.rs`) AND from
`dir.open`/`dir.create` (i32-returning imports), and is consumed by `file_read`/
`file_write` — all naming the same representation. Migrating File therefore means,
in lockstep: (a) `file_read`/`file_write` host imports + their WIR helpers take an
`externref` and downcast to the backing `PathBuf` grant; (b) `dir_open`/`dir_create`
return an `externref`; (c) the `run` wrapper takes File params as `externref`
parameters instead of `ConstI32` — which **changes the `run` export signature**,
reaching `Vm::run` and the second run site (`crates/witchy-runtime/src/runtime.rs`
lines 347 and 2380, both `get_typed_func::<(), ()>`), which must mint the
externrefs from `VmState.files` and pass them positionally via `Func::call`/
`Val::ExternRef`; (d) any File that flows into an RFC-0040 cap-gated `__export_*`
wrapper changes that export's signature too, reaching the browser shim
(`projects/coven-web/web/sandbox-src/source-sandbox.js`, which binds
`__export_export_render`) and glamour's export glue. (e) The host keeps an
**identity-keyed ownership anchor** — a `ManuallyRooted<ExternRef>` per minted File
(§8.9) — so the grant does not dangle once its `RootScope` ends. Dir/Net/Secret/
Exec/Socket/Listener stay on the i32 path (temporary two-mode mint) until Stage 3.
Verified (2026-07-05) that the export-surface change (c/d) is real; there is no
additive slice, so it is deferred to a dedicated run rather than landed half-cut.
