---
rfc: 0005
title: Unforgeable capability representation on the compiled backend
status: planned
created: 2026-06-21
superseded-by:
tracking:
---

# RFC-0005: Unforgeable capability representation on the compiled backend

> Provisional. Code blocks here are intentionally **not** tagged `witchy` so the
> doc-examples test does not try to compile them.

## Summary

On the compiled (WebAssembly) backend, every capability is represented as an
**`i32` handle** — an index into a host-side table (`VmState::dirs`,
`VmState::nets`, `VmState::files`, `VmState::sockets`, `VmState::listeners`,
`caps.secrets`). Those integers are ordinary data: they live in
WASM linear memory whenever a capability is stored in a record, captured by a
closure, or wrapped in a branded capability (RFC-0002). Linear memory is exactly
what an unsound optimizer can corrupt. witchy's security rests on memory safety,
and the in-place/uniqueness optimizer (`crates/witchy-lower/src/analysis.rs`) is, on a false
negative, an **attacker-reachable memory-corruption primitive** — the untrusted
program author chooses the source. The chain "craft an aliasing pattern that
fools the analysis → corrupt a handle in linear memory → pass a forged handle
across an import → the host honors it" is a full capability bypass that the
wasmtime sandbox does **not** stop, because wasmtime protects linear memory from
the *outside* but never from the guest's own writes.

This RFC proposes representing capabilities as **wasmtime reference types
(`externref`)** rather than `i32` handles. References cannot be stored in linear
memory, cannot be synthesized by the guest, and cannot be bit-cast from an
integer; combined with WASM code immutability, this removes memory corruption as
an attack vector against *all* capability guarantees — identity, scope, rights,
and narrowing alike. It is a parity *fix*, not a parity risk: the
interpreter already holds capabilities as unforgeable Rust values. The RFC also
specifies a set of independent hardening measures (kill the `signing@0` fallback,
trap on in-place writes, fuzz the ownership analysis, test the attenuation rules)
that are worth landing regardless of the larger change — all four have since
shipped (see the 2026-07-04 change-note), leaving the core `externref` change
itself as the open work.

## Motivation

### The threat model: the optimizer is in the TCB

In a capability-secure language the adversary is **untrusted source** — a
dependency rune, a plugin — compiled by the *trusted* witchy compiler. The
contract is that the compiler emits code that enforces the source's capability
discipline. A **miscompilation** therefore breaks the security model, and because
the adversary writes the source, any soundness gap in an optimization is not bad
luck but a *weaponizable primitive*: the attacker writes the exact aliasing
pattern that the analysis mis-classifies.

`crates/witchy-lower/src/analysis.rs` proves uniqueness so the compiler can mutate values in place
(`list.push` accumulation, clone-elision for `let`/`var`/`own` parameters). The
pass is deliberately fail-safe — it defaults to "shared/dirty" and only mutates
in place when it has *positively proven* uniqueness — which is the right bias.
But the positive-proof path is intricate (a bottom-up alias fixpoint, share-event
detection, dirty-site scanning), and a single false negative — a missed share
through a closure capture, an indirect call, or an embedded self-share like
`xs = list.push(xs, f(xs))` — causes an in-place write while a second alias to
the buffer is live. The in-place store path has **no runtime bounds check** (list
*indexing* via `$list_at` is checked; the in-place push store is not; `ensure()`
only grows the heap, it does not validate the write offset against the buffer's
capacity), so the failure mode is silent heap corruption, not a trap.

### What the wasmtime sandbox does and does not backstop

Running the compiled module in wasmtime is a real, hard boundary: a miscompile
**cannot** escape linear memory, forge an import, read host memory, or execute
native code. This is wasmtime's *stated* model, not an implementation accident —
its security documentation says explicitly that it protects the host from the
guest but does **not** protect the contents of a guest's own linear memory;
intra-guest corruption confined to linear memory is by design *not* treated as a
wasmtime vulnerability. The boundary therefore leaks precisely where **authority
is named by forgeable bytes**:

- A capability is an `i32` handle. `secret_seed_bytes`
  (`crates/witchy-runtime/src/runtime.rs`), `dir_base`, `net_allow`, the `files`
  table (RFC-0012), and the `sockets`/`listeners` tables all resolve a
  guest-supplied integer into a host-side grant, validating only that the index is
  *in range*.
- That integer sits in linear memory whenever the capability is inside an
  aggregate — a record field, a closure environment, or a branded capability
  (`Redis(net)` from RFC-0002, where the `Net` handle is a field of a heap record).
- Memory corruption can therefore overwrite a handle the program legitimately
  holds, turning it into a *different* in-range handle, and the host will honor it.

### What survives corruption, and what does not

Splitting the guarantees by whether they have a runtime backstop is the clearest
way to see the hole:

| Guarantee | Where enforced | Survives linear-memory corruption? |
|---|---|---|
| Grant *set* (which dirs/hosts/files/secrets exist at all) | host, at call time | **Yes** — a forged handle can only reach grants already in the instance's tables |
| Rights (`Dir[Read]` vs `Write`, `File[Read]` vs `Write`) | type checker only | **No** — runtime handle is the full capability |
| `as`-narrowing / `only` policy narrowing (RFC-0011) | type checker only | **No** — narrowed and full are the same handle |
| Socket/Listener identity (which live connection a handle names) | host, in-range check only | **No** — a corrupted in-range index reaches a *different* open connection the program holds (`VmState.sockets`/`listeners`, `crates/witchy-runtime/src/runtime.rs:280-283`) |

(An earlier revision of this table listed the `retain`/`without` firewall as a
fourth compile-time-only guarantee; RFC-0014 removed that feature, so the row is
gone — see the 2026-07-04 change-note.)

So the entire *intra-program* attenuation surface — the rights model and the
narrowing that make least-authority *within* a program meaningful — has **zero
runtime representation** and is defended *only* by memory safety plus the type
checker. Remove memory safety (via the optimizer) and attenuation is gone. The
*derived* capabilities are exposed too: `Socket` and `Listener` values returned
by `connect`/`listen`/`accept` are dense indices into `VmState.sockets`/
`listeners`. Corruption cannot mint a connection outside the `Net` scope, but it
can cross the data of two connections the program legitimately holds — send a
secret down the wrong socket, or read another connection's response.

### The `signing@0` footgun (independent of the optimizer)

*(Fixed since this was written — hardening #1 shipped; see the 2026-07-04
change-note. The code below is the pre-fix behavior, kept for the record.)*

`secret_seed_bytes` (now `crates/witchy-runtime/src/runtime.rs:2403`) resolved
any in-range index into `caps.secrets`, **and** fell back to returning the
signing key for handle `0` whenever a signing key was granted — regardless of
whether the program ever received a `Secret`:

```
fn secret_seed_bytes(caps: &Capabilities, handle: i32) -> Result<Vec<u8>> {
    if let Some((_, bytes)) = usize::try_from(handle).ok().and_then(|h| caps.secrets.get(h)) {
        return Ok(bytes.clone());
    }
    if handle == 0 {
        if let Some(seed) = caps.signing_key {     // <-- magic constant
            return Ok(seed.to_vec());
        }
    }
    Err(Error::msg("crypto: no secret at that handle (none granted?)"))
}
```

The only thing stopping `crypto.reveal(0)` from leaking the signing key is the
type checker refusing to make a `Secret` from an integer literal — i.e. memory
safety *is* the entire defense for the most sensitive capability in the system.

## Design

### Core change: capabilities are `externref`, not `i32`

Represent every capability value on the compiled backend as a wasmtime
**reference type** (`externref`, or a typed `(ref $cap)` under the GC proposal)
that wraps the host-side grant. The import functions change signature from
`(handle: i32, …)` to `(cap: externref, …)`, and the host resolves the
`externref` directly to its backing Rust object instead of indexing a table by a
guest integer.

This is sound against the entire threat model because of two WASM facts that
compose:

1. **Reference types cannot live in linear memory.** `externref` is storable only
   in locals, globals, tables, and GC struct/array fields — never reachable by
   `i32.store`/`i32.load`. No linear-memory corruption can read, write, forge, or
   *swap* a capability reference. There is no `i32 → externref` cast.
2. **Code is immutable and not in linear memory.** Corruption can change data but
   cannot synthesize a new call site or redirect `read(ref)` into `write(ref)`.
   The set of import calls a module can make is fixed at compile time.

Together: the only capability references a guest can ever hold are the ones the
host placed in its locals/tables at instantiation or returned from an import, in
exactly the rights the host minted. A miscompile becomes a *correctness* bug
(wrong data, a trap) and can no longer be a *security* bug.

This **raises the compiled backend to the interpreter**, which already holds
capabilities as ordinary Rust enum values (`Value::Net(..)`, `Value::Secret(..)`)
that the guest cannot fabricate. It closes a parity gap rather than opening one.

### Why per-rights references are NOT part of this RFC

An earlier sketch proposed the host mint a *distinct rights-bearing reference for
each narrowed view* (a separate `externref` for `dir as Dir[Read]`) so that
rights and narrowing gain a runtime backstop too. **This RFC deliberately rejects
that** as disproportionate.

Once capabilities are `externref`, the memory-safety route to bypassing rights is
already closed: corruption cannot forge or swap a reference, and it cannot make
the guest *call* `write(ref)` on a handle whose source only ever names `read` —
the call site does not exist in the emitted code, and no linear-memory write can
create one. After the core change, attenuation rests on **type-checker
soundness** — which is where every other guarantee in the language already rests,
and the type checker is unavoidably in the TCB.

What per-rights references would additionally defend against is a **bug in the
narrowing/rights logic itself** — typeck or codegen mistakenly emitting a `write`
call on a `Dir[Read]`. That is a code-correctness bug, not a memory-safety bug,
and the proportionate defense is a **tight test suite over the attenuation rules**
(below), not a runtime rights system the host must mint and thread through every
call boundary. Per-rights references stay on the shelf as optional defense in
depth, to revisit only if the narrowing logic proves bug-prone in practice.

### The aggregate/closure representation problem (the main cost)

Most witchy values never contain a capability, but some do: a record field, a
closure that captured a `dir`, and a branded capability (`Redis(net)`). Today
those live in the linear-memory heap, with the cap as an `i32` field. Under this
RFC a capability inside an aggregate must remain a reference — and the
reference-types proposal does not let a bare `externref` be stored in linear
memory *or in a struct/array field*; it lives only in locals, globals, tables,
and function arguments. That limitation is exactly why nesting a capability needs
one of two resolutions:

- **(A) GC structs for cap-carrying aggregates.** Represent any value that
  transitively contains a capability with WASM GC reference types
  (`struct`/`array`), so the `externref` stays a reference throughout. Cleanest
  and fully sound; the cost is that the witchy heap is currently entirely
  linear-memory, so this introduces a second (GC) representation and the lowering
  to choose between them. wasmtime **enables the GC proposal by default** and it
  ships in browsers, so this is more mature than it once was; the WIR encoder
  (`crates/witchy-wir/src/`) and `crates/witchy-lower/src/codegen.rs` would gain
  reference-typed structs for the cap-carrying subset.
- **(B) Side table of references, keyed by the value's identity.** Keep
  aggregates in linear memory but store their capability fields in a parallel
  host/guest *reference table*, with the linear-memory slot holding a table index.
  This reintroduces an integer — but into a `funcref`/`externref` *table*, whose
  entries the guest still cannot forge or corrupt from linear memory (only
  `table.get`/`table.set` touch it, and corruption of the *index* only lets the
  guest reach another reference *it already legitimately holds in its own table*).
  Weaker and fiddlier than (A): because every grant the instance holds is
  reachable, a corrupted index can still swap a narrowed handle for a fuller one
  the program holds elsewhere — so (B) does **not** fully close the *attenuation*
  gap, whereas (A), which stores the reference itself with no index, leaves no
  corruption path at all. Avoids the GC migration. (This residual weakness is the
  same one that rules out component-model `resource` handles — see *Alternatives*.)

Recommendation: **(A)** for soundness and simplicity of reasoning, scoped to only
the cap-carrying value shapes so the bulk of the heap is untouched. The choice is
the principal open question and the main implementation cost of this RFC. The
concrete migration for (A) — the `carries_cap` classification, the WIR/codegen GC
surface, the host boundary, and a staged, per-capability-type landing order that
keeps `check.sh` green at each step — is worked out in the companion design doc
`rfcs/externref-implementation-plan.md`.

### Host-side resolution

`VmState` stops indexing `dirs`/`nets`/`secrets` by a guest integer. Instead each
minted capability is an `ExternRef` wrapping the backing grant (path root +
rights for `Dir`, address-scope for `Net`, secret bytes for `Secret`). The import
implementations (`host_dir_read_len`, `host_net_connect`, `host_crypto_sign`, …)
downcast the `externref` to the concrete grant and apply the *same* host-side
scope checks they already run (`confine::resolve`, `address_admits`,
`secret_seed_bytes`'s table lookup). The grant-scope enforcement is unchanged and
already correct; only the *naming* of the capability changes from forgeable
integer to unforgeable reference.

## Additional hardening (independent, ship regardless)

These stand on their own and are worth landing whether or not the `externref`
re-architecture proceeds. They also serve as **interim mitigation** while it is
built.

1. **Delete the `signing@0` fallback.** — **SHIPPED** (see the 2026-07-04
   change-note): the `handle == 0 → signing_key` branch is gone from
   `secret_seed_bytes` (`crates/witchy-runtime/src/runtime.rs:2403`); a handle
   must name a real granted entry in `caps.secrets`, and every grant site
   populates the signing key as a normal `"signing"` entry. Closed a capability
   bypass that did not even need memory corruption.

2. **Trap on in-place writes.** — **SHIPPED**: the in-place store path
   bounds-checks the write against the buffer's real allocated size (the
   `$rc_alloc` header) and **traps** unconditionally on violation
   (`crates/witchy-wir/src/wir_helpers/mod.rs:1493-1510`), the same way
   `$list_at` traps an out-of-bounds read. This converts an ownership-analysis
   false negative from *silent heap corruption* into a *loud, parity-identical
   runtime error* — fully consistent with witchy's "anything a backend can't do
   identically is a loud error, never a silently different answer" contract. The
   cost is one compare on the in-place path, negligible against the store +
   potential `ensure()` grow.

3. **Fuzz the ownership analysis.** — **SHIPPED** via RFC-0023 (checked heap:
   canaries + loud corruption) and RFC-0037 (the correctness harness:
   differential, sanitized, coverage-guided; CI runs the heap-check fuzz sweep).
   The original rationale, kept for the record: add a differential/property
   harness that generates programs with adversarial aliasing (closure captures of
   accumulators, embedded self-shares, indirect calls, `var` write-back chains),
   runs both backends, and asserts (a) output parity and (b) no corruption via
   heap canaries. This is the direct way to find the false negatives that turn the
   optimizer into a memory-corruption primitive. Pairs with hardening #2: with the
   trap in place, a found false negative surfaces as a trap diff rather than
   undefined behavior. The compiler-assurance literature points the same way:
   differential testing of an *interpreter oracle* against a *compiler backend* is
   the standard JIT-soundness method — and witchy already has exactly that shape
   (the interpreter is the oracle, the WASM backend the subject). Augment it with
   CSmith-style generation and WASM AddressSanitizer/UBSan instrumentation plus
   heap canaries, which catch corruption a value-comparison oracle alone would
   miss (this instrumentation is the *only* realistically adoptable use of the
   "memory safety within linear memory" research — see *Alternatives*).

4. **Test the attenuation rules.** — **SHIPPED** (BUG-009, landed 2026-07-04;
   see the change-note): a focused typeck suite asserting the compile-time
   guarantees that rights and narrowing rely on: a `Dir[Read]` cannot reach
   `write`; a `Net[Connect]` cannot `listen` (nor a `Net[Listen]` `connect`); an
   `as`-narrowed handle cannot be re-widened; an `only`-scoped policy handle
   cannot be re-widened either (`crates/witchy-types/src/typeck_tests.rs` —
   `file_`/`net_`/`dir_capability_rights_and_narrowing`,
   `policy_narrowing_preserves_rights_and_cannot_rewiden`). The `without d`
   case in the original text is gone with the firewall (RFC-0014). Under the
   `externref` model these tests *are* the defense for attenuation (see "Why
   per-rights references are NOT part of this RFC"), so they are load-bearing, not
   merely nice-to-have.

5. **Interim: unguessable handles (only if `externref` is deferred).** If the core
   change is not done soon, mint handles as large random tokens rather than dense
   indices `0,1,2,…`, so that a *fabricated* integer almost never hits a valid
   grant. This is weak — it does not stop *copying* a legitimately-held handle
   into a slot where a narrowed/different one belongs, and it does not help the
   compile-time-only attenuation surface — so it is a stopgap, not a fix, and is
   subsumed entirely by the `externref` change.

6. **An independent static re-checker over the emitted WASM (VeriWasm-style).**
   Rather than *trusting* the ownership analysis, add a small offline pass that
   re-derives, from the *emitted* WASM, that every in-place store the optimizer
   introduced is within the buffer the analysis claimed unique. This is the spirit
   of VeriWasm (Johnson et al., NDSS 2021), which statically re-verifies that
   Lucet's compiled output upholds WASM's memory-isolation invariants — deployed in
   production at Fastly with no false positives. It is a *verifier*, not a test: it
   covers inputs the fuzzer never generates. Scope it to the one risky invariant
   (in-place / alias), not whole-program equivalence — full translation validation
   and CompCert-style verified compilation are out of scope for a small two-backend
   compiler. Meaningful even after the `externref` change, since it guards
   *correctness*, not just authority.

7. **Operational wasmtime hardening (adopt today, independent of everything
   above).** — **SHIPPED**: the engine `Config` lockdown is in
   `crates/witchy-runtime/src/runtime.rs:451-470` (proposals we never emit
   disabled; `reference_types`/`gc` kept on for this RFC; Spectre mitigations
   left on). Original scope: audit the embedding `Config` for defense in depth: keep Cranelift's
   Spectre mitigations on (`enable_heap_access_spectre_mitigation` /
   `enable_table_access_spectre_mitigation` are on by default — and note that
   `Config::signals_based_traps(false)` would force them off, so don't); disable
   every WASM proposal we don't emit, to shrink the codegen/runtime surface
   (`wasm_threads`, `wasm_simd`, `wasm_multi_memory`, `wasm_tail_call`, …) while
   keeping `reference_types` and `gc` on, which the core change needs; on
   Linux/x86 consider memory-protection-keys (`PoolingAllocationConfig::memory_protection_keys`)
   for intra-process isolation between instances under the pooling allocator; and
   set a `ResourceLimiter` plus fuel or epoch interruption for DoS bounds. None of
   this protects the *contents* of a guest's own linear memory — wasmtime's
   security docs are explicit on that point — which is exactly why authority must
   be moved *out* of linear memory (the core change), not defended within it.

## Alternatives

- **Do nothing; rely on the wasmtime sandbox.** Rejected: the sandbox protects
  linear memory from the outside but not from the guest's own miscompiled writes,
  and authority is named by integers that live in that memory. The sandbox is a
  real backstop for *escape*, not for *authority forgery*.
- **Per-rights, host-minted references for every narrowed view.** Rejected as
  disproportionate (see the dedicated section): the `externref` change already
  closes the memory-safety vector for rights; what remains is type-checker
  correctness, defended by tests. Kept on the shelf as optional defense in depth.
- **Unguessable handles as the primary fix.** Rejected as the primary: it raises
  the cost of *guessing* a handle but leaves *copying* a held handle and the
  compile-time-only attenuation surface untouched. Acceptable only as interim
  mitigation (#5).
- **Make the optimizer provably sound (formal verification).** Out of scope as a
  *security boundary*: even a verified analysis is a large trusted component, and
  the point of the `externref` change is to make memory safety *not* load-bearing
  for security in the first place. Fuzzing (#3) is the proportionate assurance for
  the optimizer's *correctness*.
- **Drop the in-place optimizations.** Rejected: they are central to witchy's
  performance story (`mode opt`, clone-elision). The right move is to make their
  failure mode safe (a trap, #2) and to remove authority from their blast radius
  (`externref`), not to remove them.
- **Component-model `resource` handles instead of `externref`.** Resources are the
  purpose-built capability mechanism — own/borrow + drop tracked at runtime, and
  the WASI Preview 2 lineage of "unforgeable handles" — but for *this* threat they
  are weaker than `externref`/GC. A resource handle is still an `i32` index that
  lives in the guest's linear memory, validated only against the per-instance
  handle table: an out-of-table index traps, but a corrupted *in-table* index
  still reaches another grant the instance holds — exactly the
  attenuation-re-widening attack (the same residual weakness as resolution (B)).
  Resources solve ownership ergonomics, not linear-memory corruption. Revisit them
  if witchy adopts the component model for other reasons; the unforgeable-reference
  property comes from `externref`/GC, not from resources.
- **Make linear memory itself safe (MSWasm, CHERI, always-on ASan).** Memory-Safe
  WebAssembly (segments + provenance-carrying handles) and CHERI / Arm-MTE
  substrates would catch the corrupting write *directly*, which is conceptually the
  cleanest fix. Rejected as a production guarantee: MSWasm is research-only and
  unsupported by wasmtime (or any production runtime), CHERI/MTE work (e.g. Cage)
  is hardware-gated research, and software memory-safety instrumentation carries
  tens-to-hundreds-of-percent overhead. Their practical role is *fuzzing
  instrumentation* (#3), not always-on. Removing authority *from* linear memory
  (the core change) needs no new runtime and ships today.

## Drawbacks

- **A second value representation.** Option (A) introduces GC reference types for
  cap-carrying aggregates alongside the linear-memory heap; the lowering must
  choose between them, and both must stay parity-identical with the interpreter.
  This is the real cost and the main open design question.
- **Reference-type call overhead.** Passing `externref` across imports and through
  cap-carrying call sites has some cost versus a bare `i32`; capability operations
  are not hot paths, but the branded-capability and closure cases touch ordinary
  call sites.
- **Toolchain surface.** Requires enabling reference types / the GC proposal in
  the wasmtime `Config` and in the WIR encoder/validator; raises the minimum
  feature set the compiled module depends on.
- **Migration.** Every cap-passing import signature and call site changes at once
  (`break, don't deprecate`): the `i32`-handle ABI and the `externref` ABI cannot
  coexist on the same import. This is a single coordinated cut across
  `crates/witchy-runtime/src/runtime.rs`, `crates/witchy-lower/src/codegen.rs`,
  and the WIR layer (`crates/witchy-wir/src/`).

## Prior art

- [`RFC-0002`](./0002-user-definable-capabilities.md) — sealed, unforgeable user
  capabilities at the *type* level; this RFC makes the *runtime* representation on
  the compiled backend equally unforgeable.
- [`RFC-0003`](./0003-network-address-scoping.md) — host-enforced `Net` scope; an
  example of grant-scope that is correctly enforced host-side and would be carried
  by the `externref`'s backing grant unchanged.
- [`secrets-design.md`](./secrets-design.md) — the `Secret` handle model whose
  `signing@0` fallback this RFC removes (#1).
- [`ownership-analysis.md`](./ownership-analysis.md) /
  [`performance-modes.md`](./performance-modes.md) — the in-place optimizer and
  `mode opt` whose soundness this RFC removes from the security boundary.
- The object-capability model (unforgeable references as the unit of authority);
  WASM reference types and the GC proposal (capabilities as host references the
  guest cannot synthesize); wasmtime `ExternRef`.
- wasmtime's security model — the host/guest boundary and the **explicit
  non-guarantee** that a guest's own linear-memory contents are protected
  (`docs.wasmtime.dev/security.html` and "what is considered a security
  vulnerability"); the embedding `Config` / `PoolingAllocationConfig` hardening
  knobs (Spectre mitigations, MPK, guard pages, proposal toggles, resource limits).
- VeriWasm (Johnson et al., NDSS 2021) — static verification that compiled output
  upholds an isolation invariant, deployed at Fastly; the model for hardening #6.
  Cranelift's own ISLE lowering-rule verification (Crocus / veri-ISLE, ASPLOS 2024)
  and CompCert are the heavier, less applicable points on the same spectrum.
- The WebAssembly Component Model `resource` types / Canonical ABI (own/borrow
  handles) — considered and set aside for this threat (see *Alternatives*).
- Memory-safety-within-linear-memory research: MSWasm / Iris-MSWasm (segments +
  handles) and CHERI / Arm-MTE WASM work (Cage) — surveyed and rejected as
  research-grade (see *Alternatives*); AddressSanitizer/UBSan and stack-canary
  ports (VMCANARY, WASP) as fuzzing instrumentation for hardening #3.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** The core premise is still accurate: caps are
forgeable i32 indices into dense tables (`VmState.dirs`/`nets`/`files`/`secrets`),
and the run wrapper bakes `ConstI32` handles. But the RFC was not updated as its
own hardening items shipped: #1 (signing@0 deleted, runtime.rs:2403), #2
(unconditional in-place bound traps, wir_helpers/mod.rs:1493-1510), #3 (fuzz
coverage via RFC-0037), and #7 (engine Config lockdown, runtime.rs:451-470) are
all in master. Hardening #4 is NOT done: the attenuation suite exists only for
File (rights/narrowing tests, typeck_tests.rs:215-233) — no
Net[Connect]-cannot-listen, no Dir[Read]-cannot-write (BUG-009). The
retain/without (firewall) row is obsolete — RFC-0014 removed the firewall. The
RFC is silent on Socket/Listener, which carry guest-indexed authority too
(runtime.rs:280-283). Several file citations predate the workspace split.

**Required revisions.** (1) Dated change-note recording that hardening
#1/#2/#3/#7 shipped; re-weight the motivation around the residual primitives
($rc_dup-class). (2) Delete the firewall row. (3) Fix the stale pre-workspace
paths. (4) Add File/Socket/Listener to the authority table. (5) The companion
plan is now `rfcs/externref-implementation-plan.md` (renamed from the numbered
name per rfcs/README.md numbering rules); cross-link updated.

**Verdict.** Needs-revision (keep as accepted direction). Priority: medium
overall; extracting hardening #4 (the Net/Dir attenuation tests, BUG-009) is
high priority — ~1 day, load-bearing for the externref design and useful today.

## Change note (2026-07-04): hardening status, authority surface, citations

This note records the revision that the review above required; the body edits
were applied in the same pass (the RFC is `planned`, not yet frozen).

**Hardening #1–#4 and #7 have all shipped.**
- **#1 signing@0** — deleted; `secret_seed_bytes`
  (`crates/witchy-runtime/src/runtime.rs:2403`) resolves only real granted
  entries, and every grant site populates the signing key as a normal
  `"signing"` secret.
- **#2 in-place bound traps** — the in-place store path checks the write against
  the `$rc_alloc` header's real allocation size and traps, unconditionally, on
  violation (`crates/witchy-wir/src/wir_helpers/mod.rs:1493-1510`).
- **#3 fuzzing** — RFC-0023 (checked heap: canaries, loud corruption) and
  RFC-0037 (differential/sanitized/coverage-guided correctness harness) cover
  the ownership analysis; CI runs the heap-check fuzz sweep.
- **#4 attenuation suite** — landed via BUG-009 (2026-07-04): Net, Dir, and
  `only`-policy rights/narrowing/re-widening tests now sit beside the original
  File slice in `crates/witchy-types/src/typeck_tests.rs`
  (`net_capability_rights_and_narrowing`, `dir_capability_rights_and_narrowing`,
  `policy_narrowing_preserves_rights_and_cannot_rewiden`). This was the last
  open prerequisite for beginning the externref cut.
- **#7 Config lockdown** — `crates/witchy-runtime/src/runtime.rs:451-470`:
  unemitted proposals disabled, `reference_types`/`gc` kept on for this RFC,
  Spectre mitigations left on.

Items #5 (unguessable handles) and #6 (VeriWasm-style re-checker) remain
deliberately unimplemented: #5 is subsumed by the core change; #6 is optional
depth, still worth doing after the cut.

**Re-weighted motivation.** With #2 shipped, the *known* in-place store paths
now trap instead of silently corrupting. The residual memory-corruption
primitives are the ones the traps and fuzzers have not fenced — the
`$rc_dup`-class reclamation/refcount bugs (see the 2026-07 deep-eval findings)
and any future analysis false negative on a path without a bound check. The
core argument is unchanged: authority named by integers in linear memory is one
memory-safety bug away from forgery, so the fix is to move authority *out* of
linear memory, not to keep fencing paths one at a time.

**Firewall row deleted.** RFC-0014 removed `retain`/`without`; the corruption
table and hardening #4 no longer reference it.

**Authority surface completed.** `File` (RFC-0012 grants, `VmState.files`,
`crates/witchy-runtime/src/runtime.rs:255-258`) and the *derived* `Socket`/
`Listener` capabilities (`VmState.sockets`/`listeners`, `runtime.rs:280-283`)
are now in the Summary and the corruption table. Sockets and listeners matter:
they are guest-indexed live connections, so corruption can cross the data of
two connections the program holds even though it cannot escape the `Net` scope.
The companion plan scopes their migration explicitly.

**Citations fixed** for the RFC-0018 workspace split (`src/runtime.rs` →
`crates/witchy-runtime/src/runtime.rs`, `src/analysis.rs` →
`crates/witchy-lower/src/analysis.rs`, `src/wir*.rs` →
`crates/witchy-wir/src/`).

**Status.** `proposed` → `planned`: the direction is accepted, the independent
hardening is done, and the staged migration is specified and revised in the
companion `rfcs/externref-implementation-plan.md` (2026-07-04 revision). The
remaining work is the cut itself.

**2026-07-13 implementation checkpoint.** The compiled backend's
guest-represented authority values have moved to `externref`: `File`, `Dir`,
`Net`, derived `Socket`/`Listener`, and `Secret`. `Exec`, `SecretStore`,
zero-representation ambient caps, and build caps are enforced by source typing
plus import/link grants rather than by guest-held authority handles. The
remaining RFC-0005 work is the deferred Stage 4 representation for
cap-carrying aggregates/closures and the final terminology/API cleanup around
host-owned grant objects.
