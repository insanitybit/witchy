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

## Status lifecycle

```
proposed ──▶ planned ──▶ implemented
   │            │
   ├────────────┴──▶ rejected      (decided against; kept for the record)
   │
   └──────────────▶ superseded     (replaced by a later RFC)
```

- **proposed** — written up, under discussion. Still freely editable.
- **planned** — accepted, we intend to build it. Still editable.
- **implemented** — shipped. **Frozen** from here on (see below).
- **rejected** — decided against. Frozen. Kept so the reasoning isn't relitigated.
- **superseded** — replaced. Frozen. Set `superseded-by:` to the new RFC.

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
