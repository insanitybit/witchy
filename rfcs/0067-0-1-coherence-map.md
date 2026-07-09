---
rfc: 0067
title: 0.1 coherence map — one public story for each core concept
status: proposed
created: 2026-07-06
tracking: reconciles RFC-0061/RFC-0063 with the current bug/RFC backlog before the 0.1 push
---

# RFC-0067: 0.1 coherence map

## Summary

This RFC is not a feature proposal. It is an ordering contract for the final
0.1 work.

The repo now has hundreds of local bug notes, many implemented RFCs, several
planned/in-progress RFCs, and a few branches with partial work. Treating each
item independently is no longer good enough: some bugs are symptoms of the same
unfinished migration, and some plausible fixes would make the wrong long-term
model more entrenched.

For 0.1, Witchy should be judged by a small set of public stories:

- authority is host-controlled and cannot be forged by guest data;
- public APIs are the language surface, while compiler plumbing is private;
- each core concept has one protocol (`PartialEq`, `Ord`, `Show`, `Reflect`,
  `Result`, `Option`);
- compile-time facts are structured data, not strings to parse;
- interpreter and compiled backend parity follows from one semantic model;
- docs/spec/book describe the shipped model, not the migration history.

This RFC groups the remaining work by those stories, records the dependency
order, and calls out fixes that should be considered temporary or invalidated by
destination designs.

## Motivation

RFC-0061 defines the mechanical release gate: drain blocking RFCs, close open
bugs above LOW, and complete launch hygiene. RFC-0063 raises the bar to a proud
coherence gate. The missing piece is operational: when the bug ledger and RFC
set disagree or overlap, which direction wins?

Without a coherence map, agents can make individually reasonable fixes that pull
Witchy toward different languages:

- making structural `__render` smarter while RFC-0053 is trying to make
  interpolation go through `Show`;
- hardening integer capability handles while RFC-0005 is trying to remove that
  representation;
- adding more parsing of rendered type strings while the metaprogramming model
  needs structured type facts;
- polishing duplicated task/channel helpers before deciding which concurrency
  surface is canonical;
- strengthening package/security claims before the authority model is fully
  enforced in compiled artifacts.

This RFC makes those dependencies explicit.

## Coherence Thesis

Witchy 0.1 should feel like a small language where authority, data, effects,
rendering, reflection, and packaging each have exactly one public story.

The goal is not to implement every future feature before 0.1. The goal is that
every shipped feature has a clear model, and every deferred feature is named as
future work rather than leaking as contradictory current behavior.

## Canonical Models

### 1. Authority Is Not Data

Capabilities are host authority. They must not be ordinary guest integers whose
integrity depends on linear-memory safety.

Destination design:

- RFC-0005 and `rfcs/externref-implementation-plan.md`: compiled capabilities
  become host references (`externref` / future typed refs), starting with `File`.
- Grant documents, precompiled artifacts, runtime import linking, browser host
  surfaces, and package trust checks should all be audited against that model.

Temporary work allowed:

- bounds checks, classifier fixes, and launch diagnostics around current handles.

Temporary work not sufficient:

- any destination claim that says integer handles are secure because they are
  hard to guess, validated late, or protected by unrelated memory-safety work.

### 2. Public APIs Are The Language Surface

Users should learn modules, traits, capabilities, syntax, and documented
builtins. Double-underscore names, synthetic compiler names, and backend helper
spellings should not be a second public language.

Destination design:

- direct source calls to private intrinsics are rejected outside trusted
  stdlib/compiler-generated code, or the operation is deliberately renamed and
  documented as public;
- user declarations cannot collide with synthetic/reserved namespaces in ways
  that change semantics;
- module privacy and `pub` mean what the docs say.

Invalidated fixes:

- documenting `__bytes_*`, `__erase`, or synthetic names as normal user-facing
  APIs just because they currently work.

### 3. One Protocol Per Concept

The canonical protocols are:

- equality: `PartialEq` / `Eq`;
- ordering: `PartialOrd` / `Ord`;
- display: `Show`;
- structural value facts: `Reflect`;
- absence: `Option`;
- fallibility: `Result`;
- conversion: `From` / `Into`.

Destination design:

- operators and containers satisfy the same protocol story as generic code;
- stdlib helpers such as testing assertions, JSON encoding, and collection
  helpers use those protocols instead of string-only or type-specific shims;
- every first-class std value that participates in one side of a protocol matrix
  has deliberate behavior for the rest, or a documented reason it does not.

Invalidated fixes:

- adding one-off helpers for a concrete type when a blanket protocol impl is the
  real missing abstraction;
- fixing display or equality by making backend structural helpers more special
  instead of routing through the protocol.

### 4. Rendering Means `Show`

RFC-0053 is the destination: interpolation and `show.say` should be two spellings
of the same display model.

Destination design:

- `"${x}"` uses `Show` when a value has a relevant `Show` impl, including
  derived, generic, container, and nested cases;
- structural rendering remains an implementation detail or a clearly documented
  fallback for values with no `Show`;
- generated docs and examples teach interpolation and `Show`, not retired
  `to_string`/`int_to_string` surfaces or direct `__render` calls.

Invalidated fixes:

- treating `__render` as the user-facing customization point;
- widening structural render support in a way that bypasses `Show`.

### 5. Runtime Structure Is `Reflect`; Compile-Time Structure Is Structured
`TypeInfo`

`Reflect` is the runtime value-shape protocol. Compile-time metaprogramming
needs a separate, structured view of declared types.

Destination design:

- `Mirror` has boring, complete coverage for reflectable first-class values;
- JSON's reflective encoder is explicit about where `Reflect` is the wire model
  and where a domain type should choose a custom encoding;
- `std/meta.TypeInfo` exposes structured type expressions rather than rendered
  strings such as `"List(Option(Int))"`;
- derive generators do not parse source-looking type strings for semantic
  decisions.

Temporary work allowed:

- narrow fixes to keep existing derives correct and hygienic.

Temporary work not sufficient:

- adding more ad hoc string-prefix parsing to `std/meta` as the long-term model.

### 6. Sealed Constructors Own Invariants

Invariant-bearing stdlib/domain types should establish validity at construction
time and rely on that invariant afterward.

Destination design:

- `sealed type` / smart constructors are the standard tool for `Set`, URL,
  semver, time/date, policies, and other domain values;
- public formatting/accessor/parsing code can trust the sealed invariant;
- tests check the constructor boundary, not scattered downstream revalidation.

Invalidated fixes:

- repeated defensive validation everywhere a value is consumed while leaving a
  public raw constructor open.

### 7. Async/Concurrency Has One Public Center

The repo currently has several related surfaces: `future`, `task`, `chan`,
generators/iterators, executor internals, and RFC-0059/RFC-0036 work.

Destination design:

- decide which public abstraction users reach for first;
- make the others either layered helpers, compatibility modules, or explicitly
  future/experimental;
- avoid polishing duplicated APIs until that ownership is clear.

Invalidated fixes:

- making both `future` and `task` look equally canonical without explaining their
  relationship;
- adding more executor capabilities before handle/channel unforgeability and
  boundedness decisions are settled.

### 8. Package And Registry Security Must Match The Authority Model

The package/security ambition is part of Witchy's identity. The risk is not the
ambition; the risk is overclaiming before the enforcement path is unified.

Destination design:

- package source identity, lock verification, build-step authority, grants,
  trusted publishing, WebAuthn, and registry metadata each have a single
  enforceable contract;
- docs say exactly which contracts are enforced today;
- precompiled artifacts are not presented as equivalent to source runs until
  their authority/import model is proven against RFC-0005 and the host import
  catalog.

Invalidated fixes:

- strengthening README/security prose without the corresponding gate;
- adding another package-manager trust check that duplicates, rather than
  centralizes, the source/lock/record identity contract.

## Dependency Order

Work should be sequenced in this order unless a bug is a small isolated fix with
no design coupling:

1. **Authority representation**: RFC-0005 Stage 2+ for `File` externref, then
   extend the pattern or explicitly scope the residual risk.
2. **Rendering destination**: finish the RFC-0053 generic/container follow-up so
   interpolation and `Show` truly share one path.
3. **Protocol matrix**: close protocol coverage and composition gaps for
   equality, ordering, rendering, reflection, JSON, and testing.
4. **Structured comptime facts**: replace rendered-type-string dependence in
   `TypeInfo` / derives with structured type data, or explicitly defer that as a
   known metaprogramming limitation.
5. **Public surface hygiene**: reserve/private intrinsic namespace, module
   privacy, synthetic names, stale docs, generated reference freshness.
6. **Concurrency ownership**: settle `Task`/`Future`/`chan`/executor layering and
   RFC-0036/RFC-0059 residuals.
7. **Package/Coven truth pass**: align package security, registry docs, release
   claims, and gates with the authority model that actually shipped.

Within each stream, follow verify-first triage: many backlog entries are stale.
A bug is actionable only after it reproduces or source evidence proves it still
exists on current `master`.

## Cross-Stream Invalidations

The following should be treated as hard guidance for future fixes:

| If working on... | Do not fix by... | Because... |
| --- | --- | --- |
| rendering/interpolation | expanding structural `__render` as the public customization path | RFC-0053 makes `Show` the user model |
| capability safety | making `i32` handles more obscure and calling that done | RFC-0005 removes guest-data representation |
| derives/comptime | adding more string parsing as the destination | structured `TypeInfo` is the coherent model |
| stdlib invariants | revalidating everywhere while raw constructors stay public | RFC-0065 makes constructors own invariants |
| testing helpers | adding another type-specific assertion first | protocol-based equality/display is the target |
| async/concurrency | polishing duplicate APIs equally | one public center needs to be chosen |
| package/security docs | strengthening claims before gates enforce them | release truth must match code |

Temporary fixes are still allowed when they reduce real risk, but they should be
marked as temporary and should not be used as evidence that the destination
design is complete.

## Work Intake Rule

Every future bug/RFC fix should be classified before implementation:

1. **Which canonical model does this affect?**
2. **Is this a destination fix, a temporary risk reduction, or a docs truth fix?**
3. **Does an implemented/planned RFC supersede the apparent local fix?**
4. **What evidence proves the bug still exists?**
5. **What gate prevents the old split from returning?**

If those answers are unclear, reconcile the design first. Do not let an agent
land a local patch that makes the destination model harder.

## Release Interpretation

For 0.1, not every destination design must be fully implemented. But every gap
must fall into one of these buckets:

- **shipped**: implementation, tests, spec/book agree;
- **accepted residual**: documented as a limitation/risk for 0.1;
- **deferred**: explicitly future work, not implied by current docs;
- **rejected**: no longer part of Witchy's direction.

The release is coherent when there are no silent fifth states: no accidental
private APIs, no half-flipped protocols, no docs that promise a future model as
current reality, and no security claim whose enforcement depends on an unrelated
implementation accident.

## Relationship To Existing RFCs

- RFC-0061 remains the release-versioning gate.
- RFC-0063 remains the proud-release coherence gate.
- RFC-0005 is the destination for compiled capability representation.
- RFC-0053 is the destination for rendering.
- RFC-0047 is the destination for equality.
- RFC-0044 and RFC-0054 define the current and future error-shape direction.
- RFC-0059 and RFC-0036 define the async/executor direction but still need
  public-surface reconciliation.
- RFC-0065 is the destination for stdlib/domain invariants.

This RFC does not supersede those decisions. It orders them.

## Non-Goals

- This does not require implementing every planned RFC before 0.1.
- This does not make RFCs the source of current truth; the spec and code still
  win after implementation.
- This does not forbid tactical bug fixes. It requires naming whether they are
  tactical, so they do not accidentally become the design.
