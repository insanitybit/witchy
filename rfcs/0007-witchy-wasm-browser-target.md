---
rfc: 0007
title: "witchy-WASM in the browser: a pure-compute target"
status: implemented
created: 2026-06-22
superseded-by:
tracking: |
  Shipped 2026-06-22 (commit bfc7655). The JS host-import shim
  (web/witchy-runtime/witchy-runtime.mjs) runs witchyc-compiled WASM with the
  "witchy" ABI: it provides the pure-compute imports and DENIES capabilities by
  OMISSION — imports are tree-shaken, so a capability-using rune fails with a
  LinkError; an authority-free rune instantiates when all of its non-authority
  services are in the browser catalog (proven by spike.mjs in node;
  tests/browser_shim.rs). Native-only launch/toolchain services remain omitted.
  The ABI is a stabilized contract in spec/wasm-abi.md (version 1), generated
  and checked from crates/witchy-wir/src/wir_prelude.rs. A String->String export
  ABI (__galloc + __export_<name>, codegen)
  was added under RFC-0008 (commit de0c68e) for the framework loop. DEFERRED +
  flagged (this RFC's Future work): a browser DOM/host capability — not needed
  while WASM stays pure-compute and the JS shell drives the DOM.
---

# RFC-0007: witchy-WASM in the browser: a pure-compute target

## Summary

Run witchyc-compiled WASM in the browser by writing a small JavaScript shim that
implements witchy's `"witchy"` host-import ABI with **every capability import
denied**. The result is a module structurally incapable of acquiring ambient
authority: it can compute over host-provided inputs and emit captured outputs,
but it cannot reach the network, filesystem, clock, process environment, or
secrets. The browser is not a new backend — it is the **existing compiled backend** (the same WASM
`crates/witchy-lower/src/codegen/` and `crates/witchy-wir/` already emit) plus
the **empty capability set** plus a **JavaScript host** standing in for the
wasmtime host functions in `crates/witchy-runtime/src/runtime.rs`. To make this
a stable surface rather than an internal
codegen↔runtime detail, this RFC also stabilizes and documents the `"witchy"`
import ABI as a versioned public contract.

This is the target that [RFC-0008](./0008-frontend-framework-rune.md)
("A capability-pure frontend framework (MVU over VNode)") compiles *to*: a pure
`view`/`update` core needs exactly a host that can run pure WASM and refuses to
hand it any authority. It is also the primitive that lets `projects/coven-web/`
move its sandbox renderer from vendored TypeScript onto a footprint-audited
witchy rune.

## Motivation

**The frontend is the last non-witchy surface in the stack.** The coven-web
server is 100% witchy (`std/server`); the only reason the browser client is
zero-dependency TypeScript is that there has been no way to *run* witchy in a
browser at all (PLAN §1, "Dogfood stance"). Compiled WASM exists, but nothing
provides it a host outside wasmtime. Closing that gap extends witchy to the
browser tier and lets the project dogfood the one place it currently cannot.

**It is a provable pure-compute containment primitive.** witchy's whole thesis is
that a rune's authority is a statically-computed, provable footprint (PLAN §1).
A module that imports *no* capability has an empty footprint by construction —
and if the host **refuses to satisfy any capability import**, a module that tried
to smuggle one in simply fails to instantiate. That gives untrusted code (a
sandboxed source renderer; the framework rune of RFC-0008) a containment
guarantee that is structural, not conventional: "this code cannot reach the
network/disk/clock" is true because the imports it would need are not on offer.

**It is the prerequisite for RFC-0008.** A capability-pure MVU framework is only
useful if there is somewhere to run it. Without this target the UI stays
TypeScript forever, the framework rune (PLAN §7, WS-I) cannot exist, and the
"published to coven, with a provably empty footprint, as the proof" north star is
unreachable. This RFC is WS-I prerequisite **B5**.

## Design

### The browser is the compiled backend, confined

The browser execution path is, deliberately, *not* a third backend:

- **The artifact** is the same WASM witchyc emits today — the output of
  `lower_* → assemble_wir_module → wir_encode`
  (`crates/witchy-lower/src/codegen/`, `crates/witchy-wir/`).
  No new compiler mode, no new lowering, no `--target=browser`.
- **The capability set is empty.** The launch grant that a CLI run gets from
  `--net`/`--dir`/etc. is, in the browser, simply nothing.
- **The host is JavaScript.** The wasmtime host functions in
  `crates/witchy-runtime/src/runtime.rs` —
  the functions that satisfy the module's imports — are replaced by a small JS
  shim that implements the browser-supported non-authority imports and provides
  *none* of the capability imports.

So a "browser run" is "the compiled backend, with the capability set fixed to
empty, hosted by JS instead of wasmtime." A confined variant of one backend, not
a new one (see *Parity*).

### What the shim implements — and what it refuses

witchy's compiled modules import from a single module named `"witchy"`. The
canonical catalog in `spec/wasm-abi.md` classifies every import as pure
infrastructure, capability authority, launch input, internal/toolchain service,
or runtime diagnostic, and independently marks the exact browser subset. The
shim provides no authority. Its supported subset spans deterministic
computation and marshaling, capturable output, declared user-capability policy
input, harmless reflection stubs, and abort diagnostics. Native-only argv,
compiler services, checked-heap instrumentation, and every authority-bearing
operation are omitted.

The shim **provides no capability import whatsoever**. The mechanism that makes
this a hard guarantee is WebAssembly instantiation itself: if a module imports a
function the host does not supply, **instantiation throws** with a missing-import
error. So a module whose footprint is non-empty — one that imports `Net.connect`
or `Clock.now` — *cannot be instantiated by this host at all*. The host also
omits native-only non-authority services such as argv and compiler
introspection, so its accepted surface is a deliberate subset of
footprint-empty programs. That failure is the feature: the host is a sieve that
admits no authority-bearing module.

### ABI stabilization

Today the `"witchy"` import surface is a handshake between
`crates/witchy-wir/src/wir_prelude.rs` (which declares the imports),
`crates/witchy-lower/src/codegen/` (which selects them), and
`crates/witchy-runtime/src/runtime.rs` (which satisfies them). Once a
*browser* host depends on it — and especially once third-party tooling or a
shipped framework rune does — it becomes a **public contract**. This RFC
enumerates what is frozen and versioned:

- **The import module name** `"witchy"`.
- **The host function signatures** — the name, parameter, and result shape of
  every import, plus the exact subset the browser shim implements.
- **The string-bridge protocol** — the exact pending-buffer fill sequence
  (request length, host writes bytes, guest reads), byte order, and encoding.
- **The memory/value model** — how guest pointers/lengths denote values, the
  alloc convention, and the representation of the marshaled types.

The consequence is a discipline witchy did not previously need: a compiler change
to any of these is now a **breaking change to the ABI** that must bump a declared
ABI version the shim checks. (A version mismatch is a loud refusal to
instantiate, not silent misbehavior — consistent with witchy's "loudly error
identically" rule.) This is a real constraint on compiler evolution, named
honestly under *Drawbacks*.

### Data marshaling

With no capabilities there is no ambient I/O, so the module is a pure function
`(input) -> (output)`: bytes in, bytes out, nothing observed or mutated on the
side. Values cross the boundary over the **string-bridge**. The canonical case
for RFC-0008 is a **serialized VNode**: the guest computes a view, serializes it
to a string the host reads, and the JS shell deserializes and applies it. Inputs
(the current state, an event encoded as a message) cross in symmetrically.
Because the only channel is the explicit string-bridge and there is no capability
through which to reach anything else, the module's *entire* observable effect is
the value it returns. That is what "pure-compute" means operationally, and what
makes the containment argument in *Security* tight.

### Parity

The browser runs the **same compiled WASM** as the server's compiled backend. A
different host with capabilities denied is a **confined variant** — identical
semantics, a strictly smaller authority — not a third backend with its own code
to keep in step. The two-backend parity discipline (interpreter as oracle,
compiled WASM as the run path) is unchanged: there is still one compiled artifact
with one meaning; the browser merely instantiates it under an empty grant. Any
behavior that is observable at all must already match the interpreter oracle; the
browser cannot diverge because it adds no semantics, only removes authority.

## Security

This section is the heart of the RFC: the design exists *because* of the
guarantee it provides, and that guarantee composes with the containment model
already specified in `projects/coven-web/PLAN.md` (§5) and recorded in
`projects/coven-web/SECURITY.md`.

### Deny-all-imports is the guarantee, not an implementation note

"Deny every capability import ⇒ structurally I/O-incapable" is a **first-class
security property**, not a deployment convenience. It is *why* it is safe to run
untrusted framework code and untrusted renderers:

- A pure rune's footprint is empty, provably, at compile time (coven's analyzer
  proves it touches no Net/Dir/Clock).
- The browser host satisfies no capability import, so even a module that *lied*
  — that imported authority despite a claimed-empty footprint — cannot
  instantiate.
- Therefore a successfully-running browser module has **no path to ambient I/O**.
  Its only effect is its return value over the string-bridge.

That is a static, structural statement about authority, and it is the layer
witchy contributes to the stack.

### Composition with the double-iframe sandbox (PLAN §5.2)

witchy-WASM does not *replace* the browser sandbox — it enters **inside** it,
first. The renderer/highlighter that RFC-0008 targets runs in coven-web's inner
sandbox: a null-origin frame (no `allow-same-origin`), with its own
`connect-src 'none'` CSP, reachable only over a private `MessageChannel`. Loading
a pure witchy module there yields **two independent containment proofs over the
same code**:

1. **The capability model** — compile-time, structural, an auditable empty
   footprint. The module cannot *form* the intent to do I/O.
2. **The browser sandbox** — runtime, OS/engine-enforced. Even if (1) were
   somehow wrong, the opaque origin and `connect-src 'none'` mean a fired sink
   reaches nothing sensitive (PLAN §5.2; SECURITY.md layer 2).

These two layers **fail independently**: a flaw in the compiler/footprint
analysis does not weaken the iframe, and a browser sandbox-escape does not grant
the module a capability import. That is belt-and-suspenders, and it is exactly
the doctrine coven-web already commits to — *assume every dependency is malicious*
**and** *assume XSS happens anyway* (PLAN §1). witchy-WASM strengthens the
already-belt-and-suspenders posture; it does not lean on either layer alone.

### Composition with Perfect Types (PLAN §5.1)

Perfect Types makes string→HTML sinks (`innerHTML`, `srcdoc`, `document.write`,
…) throw, so the only string→DOM path left is the browser's own sanitizer. A
VNode→DOM differ — the consumer in RFC-0008 — goes further: it builds the DOM
with `createElement` / `textContent` / `setAttribute` and **never** touches an
HTML-string sink. It therefore **cannot form the string→DOM sink Perfect Types
guards against in the first place** — there is no string to sanitize, because the
tree is constructed node by node. Framed against the two existing layers:

- The capability layer is a **static refinement** of the iframe layer: the iframe
  enforces "reaches nothing sensitive" dynamically at runtime; the footprint
  proves "holds no authority" statically at compile time. Same outcome, earlier
  and structurally.
- The VNode model is a **structural strengthening** of Perfect Types: Perfect
  Types makes the dangerous string→DOM sink *throw*; VNode→DOM removes the sink
  from the program *entirely*.

### The footprint as a trust signal

The capability footprint is the **static, auditable form of what the iframe
enforces dynamically**. Because it is auditable, coven-web can *display* it: "this
renderer's footprint is empty" becomes a trust signal shown in the UI, next to
the source it renders. This is self-similar in the way the project values — the
tool that shows footprints (coven-web) is itself built from footprint-audited
runes, and the renderer it ships is the empty-footprint proof on display.

### Tensions (named honestly)

Running WASM in a browser interacts with the project's CSP/isolation invariants.
These are real and must be stated, not glossed:

- **WASM needs `script-src 'wasm-unsafe-eval'`.** Compiling/instantiating WASM
  requires this CSP token. Inside the contained frames this is **fine**: the
  sandbox CSP is already permissive (`script-src 'unsafe-inline'`, opaque origin,
  `connect-src 'none'`; PLAN §5.3), so adding `'wasm-unsafe-eval'` there grants
  no new reach. In the **trusted parent**, however, it is a genuine CSP
  *relaxation* of the strict app-shell policy. **The rule: WASM runs in the
  sandbox/worker by default; placing it in the parent is a deliberate, documented
  decision, never the default.** RFC-0008's framework runs in the contained role,
  so it pays no parent-CSP cost.

- **COOP/COEP cross-origin isolation is pre-paid.** WASM (and `SharedArrayBuffer`,
  if ever used) want exactly the `COOP: same-origin` / `COEP: require-corp`
  cross-origin-isolated context that coven-web **already mandates on every
  response** (PLAN §5.3, "Hard invariant"; SECURITY.md layer 3). So this is not a
  new cost the RFC introduces — the isolation WASM wants is already there, paid
  for anti-Spectre reasons. It is a happy alignment, not a tax.

- **The trust shift must be named in SECURITY.md.** Today the auditable artifact
  is hand-written, zero-dependency TypeScript a reviewer can read end to end.
  Moving a renderer to witchy-WASM shifts the audit basis to: *audit the witchy
  source* + *trust the compiler* (already in the TCB) + *a reproducible build*
  (the WASM is reproducibly derived from that source) + *a provable empty
  footprint*. The parent's shipped executable artifact gets **bigger and less
  eyeball-able** (a WASM binary vs. a few KB of readable TS). That is a real
  trade — readability for a stronger structural guarantee — and it must be
  written down in `SECURITY.md` alongside the existing accepted-residual-risk
  note, not left implicit.

## Alternatives

- **Keep the frontend TypeScript forever (do nothing).** Zero new machinery, and
  the existing zero-dep TS posture is already strong. But it permanently leaves
  the browser as the one non-witchy surface, makes the framework rune (RFC-0008)
  impossible, and forecloses the "published to coven as the proof" dogfooding.
  Rejected: it cements the gap this whole initiative exists to close.

- **A witchy→JS transpiler.** Emit JavaScript from witchy source instead of
  hosting compiled WASM. Rejected: it is a *second backend* with its own
  semantics to keep in lockstep with the interpreter and the WASM backend —
  precisely the one-semantics/parity model the project refuses to fracture
  (CLAUDE.md, "The one rule: parity"). The whole appeal of the WASM path is that
  it reuses the existing compiled artifact unchanged.

- **Give browser-WASM real capabilities now** — a browser host that *does* satisfy
  capability imports (a `Dom` capability, fetch-backed `Net`, etc.). Rejected for
  now, deferred to *Future work*: it would make the browser module impure,
  dissolve the pure-compute containment primitive that is the point of this RFC,
  and require designing a whole browser host environment. The current design's
  value is precisely that it grants *nothing*.

## Drawbacks

- **ABI stabilization constrains compiler evolution.** Freezing the `"witchy"`
  import surface means a codegen change to it is now a versioned breaking change,
  not a free internal edit. This is the cost of making an internal handshake
  public; it is real and ongoing.
- **The WASM binary is a less-auditable trusted artifact.** A reviewer reads a
  WASM blob far less readily than the zero-dep TS it replaces; the audit basis
  shifts to source + compiler + reproducible build (see *Security*, trust shift).
- **CSP cost if placed in the parent.** `'wasm-unsafe-eval'` in the trusted parent
  is a relaxation; the design avoids it by keeping WASM in the sandbox, but the
  pull to "just run it in the parent" exists and must be resisted deliberately.
- **The shim is un-witchy plumbing.** The JS host shim is exactly the kind of
  hand-written, non-witchy code the dogfooding push wants to eliminate. The
  mitigation is that it is **paid once** and then hidden behind the framework: app
  authors write witchy, not shim.

## Future work

**A browser DOM/host capability — explicitly NOT designed here.** A natural next
step, *if and only if* the vision expands to having witchy-WASM drive the DOM
**directly** (deleting the TypeScript shell entirely), is a `Dom` capability and a
real browser host environment that satisfies it: the shim would then provide
capability imports backed by `document.createElement`/`fetch`/timers, and a
module's footprint would record `Dom`/`Net`/`Clock` as it does for the CLI host.

The current design deliberately does **not** do this. It keeps browser-WASM
**pure-compute** and lets the thin TypeScript shell drive the DOM (diff a
returned VNode, marshal events back in). That keeps the containment primitive
intact — the whole point is that the module grants nothing — and it is all
RFC-0008 needs. A browser host capability is therefore flagged here as the
**natural next RFC** if the project later decides witchy should own the DOM
itself; it is out of scope for this one and is not designed here.

## Prior art

- **wasmtime host functions (`crates/witchy-runtime/src/runtime.rs`)** — the
  server-side analog: the
  functions that satisfy a compiled module's `"witchy"` imports. The browser shim
  is the same role re-implemented in JS with the capability set fixed to empty.
- **The browser WebAssembly/JS import ABI** — `WebAssembly.instantiate` with an
  imports object, and the missing-import-throws behavior this RFC relies on to
  make deny-all a hard guarantee.
- **COOP/COEP cross-origin isolation** — `Cross-Origin-Opener-Policy`,
  `Cross-Origin-Embedder-Policy`, `Document-Isolation-Policy`; the context WASM
  wants and coven-web already mandates (PLAN §5.3; SECURITY.md layer 3).
- **Capability-secure JavaScript (SES / Hardened JS / Compartments)** — the JS
  ecosystem's attempt to bound ambient authority *within* JS, as a comparison
  point: this RFC achieves a stronger structural result for the *guest* by giving
  it no ambient authority at all (an empty import set), rather than by hardening a
  language that has ambient authority by default.
- [RFC-0006](./0006-compile-time-tagged-literals.md) — "Compile-time tagged
  literals," the ergonomic `html` form for authoring views that this target
  ultimately runs.
- [RFC-0008](./0008-frontend-framework-rune.md) — "A capability-pure
  frontend framework (MVU over VNode)," the consumer this target unblocks.
- `projects/coven-web/PLAN.md` (§3 architecture, §5 security model) and
  `projects/coven-web/SECURITY.md` — the containment model this RFC composes with.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below (e.g. "> 2026-07-01: clarified X").
  - The current behavior lives in spec/ and the code — NOT here.
-->
