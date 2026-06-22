---
rfc: 0005
title: Unforgeable capability representation on the compiled backend
status: proposed        # proposed | planned | implemented | rejected | superseded
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
`VmState::nets`, `caps.secrets`). Those integers are ordinary data: they live in
WASM linear memory whenever a capability is stored in a record, captured by a
closure, or wrapped in a branded capability (RFC-0002). Linear memory is exactly
what an unsound optimizer can corrupt. witchy's security rests on memory safety,
and the in-place/uniqueness optimizer (`src/analysis.rs`) is, on a false
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
narrowing, and firewalls alike. It is a parity *fix*, not a parity risk: the
interpreter already holds capabilities as unforgeable Rust values. The RFC also
specifies a set of independent hardening measures (kill the `signing@0` fallback,
trap on in-place writes, fuzz the ownership analysis, test the attenuation rules)
that are worth landing regardless of the larger change.

## Motivation

### The threat model: the optimizer is in the TCB

In a capability-secure language the adversary is **untrusted source** — a
dependency rune, a plugin — compiled by the *trusted* witchy compiler. The
contract is that the compiler emits code that enforces the source's capability
discipline. A **miscompilation** therefore breaks the security model, and because
the adversary writes the source, any soundness gap in an optimization is not bad
luck but a *weaponizable primitive*: the attacker writes the exact aliasing
pattern that the analysis mis-classifies.

`src/analysis.rs` proves uniqueness so the compiler can mutate values in place
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
native code. But the boundary leaks precisely where **authority is named by
forgeable bytes**:

- A capability is an `i32` handle. `secret_seed_bytes`
  (`src/runtime.rs:1269`), `dir_base`, and `net_allow` all resolve a guest-supplied
  integer into a host-side grant, validating only that the index is *in range*.
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
| Grant *set* (which dirs/hosts/secrets exist at all) | host, at call time | **Yes** — a forged handle can only reach grants already in the instance's tables |
| Rights (`Dir[Read]` vs `Write`) | type checker only | **No** — runtime handle is the full capability |
| `as`-narrowing | type checker only | **No** — narrowed and full are the same handle |
| `retain` / `without` firewalls | type checker only | **No** — the dropped capability still exists in the table |

So the entire *intra-program* attenuation surface — the rights model, narrowing,
and firewalls that make least-authority *within* a program meaningful — has **zero
runtime representation** and is defended *only* by memory safety plus the type
checker. Remove memory safety (via the optimizer) and attenuation is gone.

### The `signing@0` footgun (independent of the optimizer)

`secret_seed_bytes` (`src/runtime.rs:1269-1279`) resolves any in-range index into
`caps.secrets`, **and** falls back to returning the signing key for handle `0`
whenever a signing key was granted — regardless of whether the program ever
received a `Secret`:

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
RFC a capability inside an aggregate must remain an `externref`, which cannot be
stored in linear memory. Two ways to resolve it:

- **(A) GC structs for cap-carrying aggregates.** Represent any value that
  transitively contains a capability with WASM GC reference types
  (`struct`/`array`), so the `externref` stays a reference throughout. Cleanest
  and fully sound; the cost is that the witchy heap is currently entirely
  linear-memory, so this introduces a second (GC) representation and the lowering
  to choose between them. wasmtime supports the GC proposal; the WIR encoder
  (`src/wir*.rs`) and `codegen.rs` would gain reference-typed structs for the
  cap-carrying subset.
- **(B) Side table of references, keyed by the value's identity.** Keep
  aggregates in linear memory but store their capability fields in a parallel
  host/guest *reference table*, with the linear-memory slot holding a table index.
  This reintroduces an integer — but into a `funcref`/`externref` *table*, whose
  entries the guest still cannot forge or corrupt from linear memory (only
  `table.get`/`table.set` touch it, and corruption of the *index* only lets the
  guest reach another reference *it already legitimately holds in its own table*).
  Weaker and fiddlier than (A); avoids the GC migration.

Recommendation: **(A)** for soundness and simplicity of reasoning, scoped to only
the cap-carrying value shapes so the bulk of the heap is untouched. The choice is
the principal open question and the main implementation cost of this RFC.

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

1. **Delete the `signing@0` fallback.** Remove the `handle == 0 → signing_key`
   branch in `secret_seed_bytes` (`src/runtime.rs:1273-1276`); require a real
   granted entry in `caps.secrets`. VM-spawn sites that grant a lone `Secret`
   should populate `caps.secrets` instead of relying on the magic index. Closes a
   capability bypass that does not even need memory corruption.

2. **Trap on in-place writes.** Make the in-place store path (the `list_push_cap`
   fast path) bounds-check the write offset against the buffer capacity and
   **trap** on violation, the same way `$list_at` already traps. This converts an
   ownership-analysis false negative from *silent heap corruption* into a *loud,
   parity-identical runtime error* — fully consistent with witchy's "anything a
   backend can't do identically is a loud error, never a silently different
   answer" contract. The cost is one compare on the in-place path, negligible
   against the store + potential `ensure()` grow.

3. **Fuzz the ownership analysis.** Add a differential/property harness that
   generates programs with adversarial aliasing (closure captures of
   accumulators, embedded self-shares, indirect calls, `var` write-back chains),
   runs both backends, and asserts (a) output parity and (b) no corruption via
   heap canaries. This is the direct way to find the false negatives that turn the
   optimizer into a memory-corruption primitive. Pairs with hardening #2: with the
   trap in place, a found false negative surfaces as a trap diff rather than
   undefined behavior.

4. **Test the attenuation rules.** A focused typeck suite asserting the
   compile-time guarantees that rights/narrowing/firewalls rely on: a `Dir[Read]`
   cannot reach `write`; a `Net[Connect]` cannot `listen`; a `without d` scope
   cannot name `d`; an `as`-narrowed handle cannot be re-widened. Under the
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
  `src/runtime.rs`, `src/codegen.rs`, and the WIR layer.

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

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
