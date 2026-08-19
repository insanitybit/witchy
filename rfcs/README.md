# rfcs/

Design proposals and the decisions behind them. An RFC records *what we decided
and why, at a point in time*. It is a historical record, not a description of
current behavior.

## The cardinal rule

**An RFC is not the spec.** The moment an RFC is implemented, reality starts to
drift from it — the implementation differs in details, later RFCs amend it, edge
cases get discovered. This is expected and fine. The authoritative description of
what witchy does *today* lives in [`spec/`](../spec/) and, ultimately, in the
code. If an RFC and the spec disagree, the spec wins and the RFC is just history.

(Rust's RFC book says the same thing plainly: "we cannot expect every merged RFC
to actually reflect what the end result will be." Python PEPs become "a
historical document rather than a living specification" once resolved. We are
deliberately copying that separation.)

For the current implementation and release view, use the [acceptance
ledger](./0087-acceptance-ledger.md) and the [release-readiness
ledger](../RELEASE-READINESS.md); both record executable evidence rather than
RFC intent.

## Canonical language and standard-library RFCs

Most numbered RFCs are narrow historical decisions. The canonical language
story is intentionally smaller. Start with these capstones, then follow their
`predecessors` only when the design history or implementation evidence matters:

| Canonical RFC | Owns | Principal historical decisions |
|---|---|---|
| [RFC-0125](0125-core-language-contract.md) | Core data model, functions, modules, traits, patterns, errors, and expression boundaries | 0021, 0022, 0042, 0045, 0046, 0048-0050, 0052, 0054, 0056, 0062, 0065, 0078, 0081, 0097, 0098, 0113, 0123; 0084 stays deferred |
| [RFC-0126](0126-capability-effects-contract.md) | Capabilities, effects, refinement, grants, host bindings, and effectful stdlib rules | 0002, 0003, 0005, 0009, 0011-0014, 0020, 0038, 0040, 0057, 0060, 0068, 0076, 0077, 0091, 0102, 0103, 0106, 0121; 0085/0086 stay deferred |
| [RFC-0127](0127-ownership-and-opt-mode.md) | Value ownership, mutation, qualifiers, `mode opt`, references, lifetimes, layouts, and no-copy contracts | 0024-0030, 0033, 0034, 0043, 0051, 0062, 0064, 0083, 0087-0090, 0110-0112, 0122; 0114 stays deferred |
| [RFC-0128](0128-regions-and-reclamation.md) | `region:`, arenas, RC fallback, copy-out, and predictable reclamation | 0016, 0024, 0035, 0051, `regions.md` |
| [RFC-0129](0129-concurrency-tasks-and-channels.md) | `async`/`await`, tasks, typed channels, structured concurrency, and parallel workers | 0032, 0036, 0055, 0059, `concurrency-design.md` |
| [RFC-0130](0130-generators-and-iterators.md) | `Iter`, `gen fn`, `yield`, resumable frames, and `FromIterator` | 0052, 0059, 0074 |
| [RFC-0131](0131-reflection-and-comptime.md) | `Reflect`, `Mirror`, `TypeInfo`, `comptime`, quotation, derives, and tagged literals | 0006, 0053, 0065, 0069, 0080 |
| [RFC-0132](0132-runtime-dynamic.md) | Explicit owned `Dynamic` values, descriptors, checked invocation, and dynamic trait queries | 0081, 0082 |
| [RFC-0133](0133-standard-library-contract.md) | Protocols, collections, errors, module families, target availability, and stdlib maturity | 0021, 0031, 0044, 0047, 0048, 0053, 0054, 0065, 0069, 0074 |

These capstones consolidate navigation and long-term product intent. They do not
turn the RFC directory into a mutable specification. Current behavior remains in
`spec/`, source, and executable evidence. Compiler architecture, release
engineering, contributor process, Glamour, Coven, packaging, and deployment RFCs
remain outside this language/stdlib subset and retain their own histories.

RFC-0124 is intentionally not reused. Its discarded-collection optimization was
absorbed into RFC-0123 and then into RFC-0125.

## Status lifecycle

```
proposed ──▶ accepted ──▶ planned ──▶ implemented
   │            │            │
   ├────────────┼────────────┴──▶ deferred      (parked with a revisit trigger)
   ├────────────┴───────────────▶ rejected      (decided against; kept for the record)
   └────────────────────────────▶ superseded    (replaced by a later RFC)
```

- **proposed** — written up, under discussion. Still freely editable.
- **accepted** — the decision or direction is approved, but this is not a claim
  that the design is fully shipped. `tracking:` records implementation progress
  or explains why the RFC is an ongoing policy rather than a finite feature.
- **planned** — accepted and committed to a future implementation cut. Still editable.
- **deferred** — consciously parked and not currently release-blocking.
  `tracking:` records why and the evidence or event that should revive it.
- **implemented** — shipped. **Frozen** from here on (see below).
- **rejected** — decided against. Frozen. Kept so the reasoning isn't relitigated.
- **superseded** — replaced. Frozen. Set `superseded-by:` to the new RFC.

An accepted RFC may move directly to implemented when it ships without a
separate planning phase. A deferred RFC may return to proposed, accepted, or
planned when its revisit condition is met; changing the status and appending a
dated note makes that revival explicit.

## Freeze + supersede, don't rewrite

Once an RFC is `implemented`/`rejected`/`superseded`, **don't substantially edit
it**. A changed decision is a *new* RFC that supersedes the old one — not an edit
to the old text. This keeps the wrong turns visible instead of erasing them.

Two pragmatic exceptions (the ADR "living document" concession — strict
immutability tends to lose to reality):

- Fixing the `status:` / `superseded-by:` fields is always allowed.
- Appending a **dated** change-note (`> 2026-07-01: …`) is allowed. Silent edits
  to the body are not.

## Naming

- New RFCs: `NNNN-slug.md`, zero-padded, allocated in order (`0001-…`, `0002-…`).
- Imported historical plans keep their original filename and carry
  `status: implemented` (or `superseded`). RFC numbers are only for new,
  single-decision RFCs going forward.

Start from [`TEMPLATE.md`](./TEMPLATE.md).
