---
rfc: 0102
title: "Portable roots: the Fetch capability, host grant menus, and the provider doctrine"
status: draft
created: 2026-07-22
tracking: >
  Draft. Phase order: (1) doctrine + menu documents, (2) the Fetch root with
  native/interpreter/fixture providers and the std/http client cut-over,
  (3) the browser Fetch provider + playground wiring, (4) browser host
  expansion for existing families (argv, secrets, vm workers), (5) menu
  repairs (Env, Exec, Console) as independently landable slices.
predecessors:
  - "[0002](0002-user-definable-capabilities.md) (user-definable capabilities — attenuation above roots; never mint from nothing)"
  - "[0012](0012-file-capability.md) (File — the grantable-root-plus-derived dual-status precedent)"
  - "[0013](0013-capability-grant-documents.md) (grant documents — the launch half of a host menu)"
  - "[0038](0038-grantable-user-capabilities.md) (grantable user capabilities — root granting and the footprint axis)"
  - "[0040](0040-grantable-caps-on-exported-entrypoints.md) (exported entrypoints — per-platform variance at the root)"
  - "[0057](0057-capability-policy-constructors.md) (policy constructors — the value-policy vocabulary)"
  - "[0091](0091-browser-virtual-capabilities.md) (browser opt-in capability host — per-host providers; the Net deferral this RFC resolves as Fetch)"
  - "[0092](0092-trusted-application-executables.md) (trusted executables — the binding-plan grant shape Fetch extends)"
related:
  - "[0086](0086-capability-gated-native-extensions.md) (the only third-party door to new primitive authority)"
  - "[0032](0032-multi-core-execution.md) (zero-authority workers; capability transfer to VMs recorded here as future work)"
---

# RFC-0102: Portable roots — the Fetch capability, host grant menus, and the provider doctrine

> Provisional syntax. Code blocks here are intentionally **not** tagged
> `witchy` so the doc-examples test does not try to compile them.

## Summary

One new root capability — `Fetch`, request/response HTTP-client authority —
plus the doctrine that explains it and the data model that publishes it.

- **Doctrine.** Root capabilities are what a *host* can honestly grant;
  libraries only attenuate (RFC-0002). A new root is admitted only when it is
  *host-primitive somewhere*: some host provides it natively AND it cannot be
  expressed as an attenuation of existing roots on every host where it should
  exist. Capabilities are universal types with **per-host providers**;
  platform difference lives in grants, never in the language surface.
- **`Fetch`.** The browser has real HTTP authority (`fetch()`) but no raw
  sockets, so network access on the web cannot be an attenuation of `Net` —
  it must be a root. `Fetch` is origin-allowlisted, derivable from
  `Net[Connect, Tcp]` on hosts that have Net (the `File`-from-`Dir`
  precedent), and provided natively by the CLI host, the interpreter, the
  browser host, and a deterministic fixture provider for pinned differential
  tests. `std/http`'s client moves to `Fetch` in one cut; its server half
  stays on `Net[Listen]` and is honestly non-portable.
- **Host grant menus.** Each host publishes the capability families and
  rights it can grant as a data document. Tooling answers "will this rune run
  on that host?" as `footprint ⊆ menu`. The book classifier, coven's
  `browser_runnable` field, the grant-document cross-check, and the
  trusted-exe binder all derive from menus instead of maintaining six
  partial encodings of the capability list.
- **Browser host expansion.** The opt-in browser host additionally provides
  argv, `SecretStore`, and the `vm.*` worker imports (sequentially — exactly
  the semantics the parity oracle defines), so those example classes become
  runnable in the playground.
- **Menu repairs.** Three existing roots are brought up to the doctrine's
  standard as independently landable slices: `Env` gains granted-name
  narrowing, `Exec` gains in-language narrowing and a grant-document section,
  `Console` gains `[Read, Write]` rights (introducing stdin authority).

## Motivation

Three converging pressures:

1. **The playground refuses honest programs.** Running a `vm.serve` example
   reports `capability 'vm_serve_run' is not available in the browser
   playground`; every `Net` example is equally dead. The browser host
   (RFC-0091) backs `Console`/`Clock`/`Env`/`Dir` and traps everything else.
   The goal is to maximize the set of book examples that genuinely run in
   the browser — not to mark examples non-runnable.
2. **Audit granularity stops at `Net`.** A library that talks to one HTTPS
   API must take a raw `Net`, so it audits indistinguishably from one that
   can dial anywhere. RFC-0002 solved this *above* the roots (brands), but on
   the web there is no `Net` to attenuate — the authority gap is *below* the
   brand layer, and only a new root can fill it.
3. **The capability menu has no single source of truth.** At least six
   places partially encode "what capabilities exist and who can grant what":
   `BUILTIN_TYPE_NAMES` (typeck), the rights parsers (`typeck/cap_rights.rs`),
   the grant-document sections (`witchy-caps/src/grants.rs`, which itself
   notes some capabilities have no section and sit outside the cross-check),
   the trusted-exe binder's match (which recognizes `NativeLoader`, a name
   absent from `BUILTIN_TYPE_NAMES`), the book classifier's hardcoded
   `BROWSER_CAP_FAMILIES`, and `std/policy`'s impl list. Each answers from
   one angle; none is authoritative.

## Doctrine

### Roots are host-provided; libraries attenuate

A root capability is authority to touch the world. It must bottom out in host
code — the runtime kernel's import table or the browser host — so only a host
can originate it. Libraries compose and attenuate (RFC-0002's sealed brands:
minted only by consuming roots, transparent to the footprint analyzer,
"never mint from nothing"). The single third-party door to new *primitive*
authority is RFC-0086's capability-gated native extensions, which is itself a
root grant and audits as maximal taint. This is the invariant that makes
`witchy caps` trustworthy: every authority traces to an explicit host grant.

### The root-admission criterion

A new root family is admitted only if **both** hold:

1. **Host-primitive somewhere.** At least one host can provide it natively.
2. **Not universally derivable.** It cannot be expressed as an attenuation of
   existing roots on every host where it should exist.

`Fetch` qualifies: the browser provides HTTP natively and has no `Net` to
attenuate. A future WebSocket root would qualify by the same test (the
browser has the WebSocket API; native can speak WS over Net). A SQL
capability does **not** qualify — it is derivable from `Net`/`File` on every
host — and stays an RFC-0002 brand. This criterion is the guard that keeps
the menu small.

### Three-tier expression

A root's authority is expressed at up to three layers, and the menu's best
practice (exemplified by `Net` and `Dir`) uses all three:

1. **Type-level rights** — verb classes checked statically
   (`Dir[Read]`, `Net[Connect, Tcp]`). Ship a right only when something
   enforces it: `Udp`/`Uds` are currently type-level markers with no runtime,
   and Capsicum's ~80-flag rights vocabulary is the cautionary tale.
2. **Value-level policy** — instance scoping enforced at runtime, built with
   RFC-0057 policy constructors and monotone narrowing (`net.restrict`,
   `Dir` subtree confinement, pinned dials).
3. **Grant shapes** — CLI flags, RFC-0013 grant-document sections, RFC-0092
   trusted-exe bindings, build grants, and the browser menu.

### Providers, and determinism classes

A capability type is universal; each host supplies a **provider**. RFC-0091
set the precedent: browser `Clock` is real wall time, browser `Dir` is a
per-run in-memory tree — same types, honest per-host semantics. Providers
fall into two classes:

- **Deterministic** (fixture-backed, page-supplied, seeded): runs are
  reproducible, so examples can be output-pinned and differentially
  verified. `Rand` under `WITCHY_RAND_SEED` is the existing precedent.
- **Nondeterministic** (real time, real network): programs run but their
  output is not pinned — the `browser_runnable`-but-not-`runnable` tier.

Every root that can perform I/O gets a **fixture provider** on both backends
so the differential suite can adjudicate it. Mock providers are the *testing*
story, not the portability story.

### Portability needs no new language surface

A program is portable to a host iff its footprint ⊆ that host's menu — a
static check the analyzer can already answer. Variance lives at the edges:

- **Entrypoints** (RFC-0040): one source may expose a native `main` taking
  `Net` and a browser export taking `Fetch`; each entrypoint carries its own
  footprint.
- **Optional capabilities**: `main(console: Console, net: Option(Net))`
  degrades gracefully where a host grants less.
- **Trait generics**: a shared core written against a bound
  (`fn sync[C](client: C, ...)`) with each entrypoint injecting its concrete
  capability; monomorphization keeps every specialization's audit exact.

Comptime target predicates (Rust-style `cfg`) are **rejected** for
signature-forking: witchy expresses platform authority as grant-time
capability availability, not link-time symbol availability, so the role cfg
plays in Rust is already played by grants — and cfg'd signatures would fork
the audit per target, spread transitively through callers, and multiply the
parity matrix. (See Rejected alternatives.)

### Deliberate non-capabilities

Recorded as doctrine so they read as design, not classifier accident:

- **`vm.*` workers are authority-free.** Workers are minted with zero
  capabilities; parallelism is pure compute, bounded by host resource
  limits, not authority. (Capability transfer to workers is future work —
  see below.)
- **argv is host data, not authority** — a value the host passes, like the
  environment *map contents* once `Env` is granted.
- **Console is write-only today**; stdin authority does not exist. (Repaired
  below — as a *right*, not an ambient.)

## The `Fetch` root

### Type and contract

`Fetch` is request/response HTTP(S)-client authority. No type-level rights
in v1 (per the enforce-before-you-ship rule); scoping is value-level by
origin. The operation contract, identical on every provider:

```
fetch.send(request) -> Result(Response, HttpError)
```

with `Request`/`Response` being `std/http`'s existing types, and:

- **Origins, not addresses.** A `Fetch` carries an origin allowlist
  (`scheme://host:port`). `send` to a non-allowlisted origin is a uniform
  denial error on every provider, checked before any I/O.
- **No automatic redirects, no location disclosure.** A redirect response is
  a uniform `HttpError` on every provider. Rationale: the browser cannot
  reveal cross-origin redirect targets (an opaque-redirect is all `fetch()`
  yields), and a provider that silently follows redirects would either leak
  requests to non-allowlisted origins or diverge from providers that check
  every hop. The honest intersection — redirect is a loud, uniform error —
  is the v1 contract everywhere, including native (which could do more but
  must not, for parity).
- **Buffered bodies** (`String`/`Bytes`), no streaming in v1.
- **Uniform timeout error**; the limit is host policy, the error shape is
  contract.

### Value policy

RFC-0057-style constructors and monotone narrowing:

```
let api = fetch.only(["https://api.example.com"])   // narrow to origins
```

Narrowing is intersective and irreversible, like `net.restrict`. RFC-0002
brands compose above as designed: `capability GithubApi from Fetch`.

### Derivation from Net

On hosts that have `Net`, a `Fetch` is derivable by consuming (narrowing
from) a `Net[Connect, Tcp]` — the same dual status `File` has (host-grantable
directly, and derivable from `Dir`). The derived `Fetch`'s origin allowlist
is bounded by the source `Net`'s address allowlist; derivation never widens.
This makes `Fetch` the *portable waist*: native code holding `Net` derives
`Fetch` and calls the same libraries a browser entrypoint calls with a
granted `Fetch`.

### Providers

- **Native (CLI, trusted-exe) and interpreter**: a host-side Rust HTTP/1.1
  client over the existing confinement/pinned-dial machinery (the
  DNS-rebinding TOCTOU closure `std/http` documents today moves host-side
  with it). Both backends share the implementation; the differential suite
  adjudicates the contract.
- **Browser**: real `fetch()`. The origin allowlist is enforced by the host
  provider *before* issuing the request — CORS is defense in depth, not the
  security boundary. Requests are sent without credentials. `redirected`
  responses surface the uniform redirect error. The honest limitation —
  browser `Fetch` reaches only the CORS-visible web — is documented, and
  playground examples target CORS-enabled or playground-hosted endpoints.
- **Fixture (both backends)**: a deterministic origin→scripted-responses map
  from the shared capability-fixture schema
  (`{ argv, env, secrets, dir, fetch, clock, exec }`). This is the pinned,
  differential-tested tier, and the in-language mock precedent is the
  interpreter's in-memory `Dir`.

Real-`fetch()` runs are nondeterministic: `browser_runnable`, not pinned.

### Grant shapes

- Grant documents gain a `[fetch]` section (origin allowlist), bound
  precisely to the `Fetch` family (the `[secrets]`→`SecretStore`-only lesson,
  BUG-117).
- The CLI gains a `--fetch <origin>` launch flag alongside `--net`.
- Trusted executables gain `[targets.trusted-exe.fetch]` with
  `from = "allow", origins = [...]` (and `from = "system"` as the explicit
  unrestricted form), mechanical per RFC-0092.
- The browser menu lists `Fetch` with a host-configured allowlist.

**Prior art in-repo:** `BuildCap::Net(Vec<String>)` is documented as "fetch
from this allow-list of hosts" — the build platform already ships a
Fetch-shaped authority. Unifying `BuildNet` into a build-granted narrowed
`Fetch` is recorded future work (one cut when taken, per break-don't-
deprecate).

**External prior art:** `wasi:http/outgoing-handler` is this capability in
the component-model ecosystem, and jco already implements it over browser
`fetch()`. Fetch's contract should stay close enough that a component-model
host could satisfy it directly.

### std/http splits

The client API moves to `Fetch` in one cut — `http.get(fetch, url)` — and
`Net`-holding callers derive. The server half stays on `Net[Listen]`,
honestly non-portable. The split is the API telling the truth: HTTP clients
are portable, HTTP servers are not.

## Host grant menus

Each host publishes a **menu document**: the capability families, rights,
and grant forms it can honestly provide. Illustrative shape:

```
# menus/browser.toml
host = "browser-playground"
[grants]
console = { }
clock   = { }                      # real time: nondeterministic
env     = { source = "page" }      # page-supplied map
dir     = { source = "memory" }    # per-run in-memory tree
fetch   = { origins = "host-configured" }
secrets = { source = "page" }
argv    = { source = "page" }
vm      = { mode = "sequential" }
# absent = denied: net, exec, native
```

Consumers, replacing today's six partial encodings:

- `witchy caps <rune> --menu <host>` answers portability statically and
  names the violating capabilities.
- The book classifier derives `browser_runnable` from the browser menu
  (deleting the hardcoded `BROWSER_CAP_FAMILIES` and the ad-hoc
  `reads_argv`/`uses_workers` exclusions as their providers land).
- The playground host configures itself from the same document — one source
  of truth for what it claims and what it provides.
- Coven's `browser_runnable` metadata becomes a derived fact.
- The grant-document cross-check and trusted-exe binder validate against the
  menu instead of private lists.

"Platform" is deliberately **not** a language concept and not a closed enum:
the set of hosts is open (embedders publish their own menus), and the
language already states every program's platform requirement in its own
vocabulary — the footprint. This is the WASI-worlds shape: a world/menu is
published host data, checked against typed requirements.

## Browser host expansion (existing families)

Providers for already-specified families, closing RFC-0091 deferrals:

- **argv**: page-supplied argument array. Doctrine: argv is host data; no
  capability involved. The classifier's `reads_argv` exclusion drops from
  `browser_runnable`.
- **`SecretStore`**: page-supplied named-secret map (the glamour secret
  plumbing is precedent). `use-only` semantics preserved.
- **`vm.*` workers**: implement `vm_serve_run` and `vm_par_map_bytes_write`
  in the browser host by instantiating a second zero-authority
  `WebAssembly.Instance` of the same module and driving the `__call2`
  trampoline **sequentially**. The determinism doctrine defines these ops'
  semantics as the single-VM sequential result, so a sequential browser
  implementation is exactly correct — no Web Workers or SharedArrayBuffer
  required. The `uses_workers` exclusion drops. This closes the reported
  `capability 'vm_serve_run' is not available in the browser playground`.

## Menu repairs

Three existing roots repaired to the doctrine's standard. Each is an
independently landable slice applying patterns the menu already contains.

### Env: granted-name narrowing

Holding `Env` today means reading *every* variable — credentials included —
with no rights, no narrowing, no grant section. Meanwhile `BuildCap::Env` is
already a granted-name map. Lift that design to runtime: value-level
narrowing (`env.only(["HOME", "LANG"])`), an `[env]` grant-document section
with a name allowlist, and trusted-exe bindings that name their variables.
Bare `Env` stays full-access (bare-`Net` convention), so existing programs
are untouched; the win is that libraries can now be handed less.

### Exec: complete the layers

Exec allowlisting exists at the trusted-exe and build layers but not
in-language, and runtime grant documents cannot grant Exec at all. Mirror
`Net`: a policy constructor / narrowing (`exec.only(["git"])`) and an
`[exec]` grant-document section. No type-level verbs (running is running).

### Console: Read and Write rights

`Console[Read, Write]`, Dir-style; bare `Console` = full rights, existing
code untouched. This introduces stdin authority as a *right* — a
`Console[Read]` holder can capture typed input (passwords), categorically
different trust from a logger, and the distinction must exist the moment
input exists. Providers: native = real stdin; fixture/browser =
page-supplied input lines (the seeded-`Rand` pattern). Interactive input is
nondeterministic → runnable-unpinned tier. No `[Error]` right until stderr
is separately addressable (enforce-before-you-ship).

## Rejected alternatives

- **A restricted `Net` on the web** (browser "Net" that secretly does
  `fetch()`): Net's contract is a byte stream; an emulation injects headers,
  buffers bodies, and drops raw-byte semantics — silent divergence by
  construction, the exact thing the parity rule forbids. Net is simply never
  granted in the browser.
- **A web-only `Fetch`**: forks the language surface per platform and breaks
  the differential story. Universal type, per-host providers.
- **Mock-Net as the portability story**: fixtures cannot make examples *do*
  anything real, and a fixture-backed "network" masquerading as live is the
  wrong default for a playground. Fixtures remain — as the pinned testing
  tier, where determinism is the point.
- **`Fetch` as a pure RFC-0002 brand**: brands attenuate; they cannot
  conjure. The browser has no `Net` to consume, and a bare (RFC-0038)
  brand has no I/O ops to call — all effects bottom out in the host import
  table, which is precisely what lacks an HTTP primitive today.
- **Comptime target `cfg`**: forks meaning per target (the audit becomes
  target-indexed), spreads transitively (a caller of a cfg'd signature must
  itself fork), multiplies the parity matrix, and is redundant — grants
  already carry the platform difference at the correct layer, and
  entrypoint-level variance (RFC-0040) covers per-target footprint shaping.
  Coherent to express via structured comptime later if a need appears that
  entrypoints cannot serve; deferred with this record.
- **Enums over capabilities** for portability: the authority-taint guards
  deliberately resist authority hiding inside data, and an audit must assume
  a union's widest arm — `FromNet(Net) | FromFetch(Fetch)` audits like raw
  `Net`. Trait generics keep the audit exact; use those.
- **Platform-forked root families** (a web `Fetch` next to a native
  `NetFetch`, on the `BuildNet` model): fork a root only when the *contract*
  differs, not the platform. `BuildNet` exists because build grants have a
  different trust model (consumer-declared per-dependency), and even that
  fork is slated for unification.

## Future work

- **`BuildNet` unification** into build-granted narrowed `Fetch` (one cut).
- **WebSocket root** — passes the admission criterion (browser-primitive;
  native over Net); take it when a consumer exists.
- **Streaming bodies and verb rights** on `Fetch` — when enforced.
- **Revocation / membranes**: a granted capability currently lives until
  program exit; long-lived servers eventually want E-style membranes or
  seL4-style derivation-tree revoke.
- **Powerbox granting**: the playground interactively asking the user to
  grant a capability mid-run, instead of static page configuration.
- **Move-only capability transfer to workers**: workers stay zero-authority
  in this RFC. The principled extension is transfer-by-move at spawn
  (exploiting the ownership axis: one holder at any time, so no single
  authority's effects self-interleave), with E's vats / Fuchsia's routing as
  prior art and `BuildCap` minting as the in-repo grant-at-spawn precedent.
  Any such step must re-justify the parity oracle first.

## Implementation phases and evidence

1. **Menus**: publish `menus/{native,browser,trusted-exe,build}.toml`; derive
   the book classifier from the browser menu with a test proving it equals
   the current hardcoded behavior before any widening.
2. **`Fetch` core**: typeck family + origin policy + `Net` derivation; native
   + interpreter providers sharing one host implementation; fixture provider;
   differential tests over the contract (allowlist denial, redirect error,
   timeout shape); `std/http` client cut-over.
3. **Browser `Fetch`**: the `fetch()` provider with host-side allowlist;
   grant shapes (`[fetch]`, `--fetch`, trusted-exe binding); a playground
   test running a real book example against a playground-hosted endpoint.
4. **Browser expansion**: argv, `SecretStore`, `vm.*` sequential port; drop
   the corresponding classifier exclusions; re-bless `book/examples.json`
   and record the `browser_runnable` delta (the acceptance metric: strictly
   larger, with zero examples made less runnable).
5. **Menu repairs**: Env, Exec, Console as three independent branches, each
   with both-backend differential coverage.

Every phase lands through the serialized gate; parity-sensitive slices
(interpreter fixture providers, typeck family) take adversarial review.

## Compatibility

`std/http`'s client changes signature in one cut (callers derive `Fetch`
from their `Net`); no deprecation layer. Bare `Console` and bare `Env`
retain full rights, so existing programs compile and behave identically.
New grant sections are additive. The menu documents are new files; the
classifier change is behavior-preserving until providers land.
