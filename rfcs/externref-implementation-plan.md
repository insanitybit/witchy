---
rfc: 0005-impl
title: Externref capability core — implementation design (companion to RFC-0005)
status: design
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
types (post-`typeck`), true iff a value of `ty` can transitively hold a capability:

- a capability type itself (`Dir`, `Net`, `File`, `Console`, `Exec`, `Secret`,
  `SecretStore`, `Clock`, and every `capability`/`grantable capability`
  declaration, incl. the RFC-0002 branded forms like `Redis(net)` and the
  RFC-0039 UI tokens);
- a record/`type` variant with any cap-carrying field;
- a tuple with any cap-carrying element;
- a closure whose captured environment has any cap-carrying capture;
- `Option`/`Result`/`List`/`Dict` **only** if their element/payload is
  cap-carrying (a `List(Int)` is not; a `List(Net)` is — rare but must be handled
  or explicitly rejected, see §7).

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

## 5. Instantiation: minting the root capabilities

At `Runtime::spawn`, the host mints one `externref` per granted capability
(wrapping its `Arc<Grant>`) and passes them as the `run`/`__export_*` wrapper's
externref arguments in declaration order — exactly where the current code passes
`i32` handles `0,1,2,…`. A cap the program was not granted is simply not minted
and the corresponding import is not linked (unchanged deny-by-omission). The
`--dir`/`--net`/`--secret`/`--signing-key` grant plumbing (`main.rs`) is unchanged
above the mint point; only the handle-vs-externref hand-off at the boundary moves.

## 6. Staging (how an "all-or-nothing" cut is still reviewable)

The ABI can't be half-i32-half-externref *for one capability*, but it **can** be
migrated one capability TYPE at a time if the classification and the boundary are
done first. Proposed order, each stage its own PR, `check.sh` green at each:

1. **Infra, no behavior change.** Add the `WirTy::Extern`/`GcRef` variants, the
   type-section encoder, and the `StructNew/Get/Set` nodes — all unused. Add
   `carries_cap`. Prove the encoder round-trips a hand-built GC-struct module
   (a `wir_encode_tests` fixture). Nothing lowers to them yet. GREEN.
2. **`Console` end to end** (the simplest cap: rights-less, never nested in the
   common programs). Mint it as an `externref`, its `print`/`print_int` imports
   take `externref`. This proves the boundary + the mint path on the least-risky
   capability. GREEN (all Console programs, both backends).
3. **The scalar root caps** (`Dir`, `Net`, `File`, `Secret`, `SecretStore`,
   `Clock`, `Exec`) — each as an `externref` param, imports downcast. Still no
   aggregates. GREEN after each.
4. **Cap-carrying aggregates** — records/tuples/branded caps/UI tokens to GC
   structs; then **closures** capturing caps. This is the hard stage; the
   `carries_cap` classification and GC-struct lowering land here. GREEN.
5. **Delete the i32 handle machinery** — `VmState.dirs/nets/secrets`, the index
   arithmetic, the `*_handle` conventions. The suite staying green with them gone
   is the proof the migration is total.

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

## 9. Definition of done (unchanged from RFC-0005)

All capability imports take `externref`; the i32 handle tables are deleted; parity
is green on both backends; the differential fuzzer (augmented with adversarial
aliasing over cap-carrying aggregates) finds no diffs; the attenuation suite (§4
of RFC-0005, already comprehensive) stands as the runtime backstop. At that point
a miscompile is a *correctness* bug (wrong data, a trap), never a *security* bug —
the thesis of RFC-0005.

---

<!--
  This is a DESIGN doc (status: design), not a frozen decision. It refines the
  implementation of RFC-0005's already-chosen approach (A). Edit freely until the
  cut begins; then it tracks the actual migration.
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
