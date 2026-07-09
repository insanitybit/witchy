---
rfc: 0074
title: "Container API symmetry: the contains decision, list.remove, and the deferred tail"
status: implemented
created: 2026-07-07
tracking: quality audit 2026-07-07 (scratch/audit-2026-07-07-quality/REPORT.md, F5);
  implemented on master 515d190c (list.remove + differential tests + method form)
  and the contains_key rationale doc-comment (std/dict.witchy); deferrals stand as recorded
related:
  - "0044 (std error policy — owns failure *shapes*; rule 5 already blesses documented clamping)"
  - "0049 (naming lexicon, implemented/frozen — owns the conventions; this RFC decides cases it did not cover)"
  - "0070 D5 (kill list — same 'one spelling per concept' spirit; no overlap in items)"
---

# RFC-0074: Container API symmetry

## Summary

The 2026-07-07 quality audit swept the container modules (`list`, `dict`,
`set`, `string`, `bytes`, `iter`) for cross-module asymmetries. Most of what it
found is already owned (RFC-0044 owns failure shapes; RFC-0049 owns names;
BUG-508 owns `split_once`'s sentinel). Three items are genuinely undecided.
This RFC decides them: **keep** `dict.contains_key` (rejecting the rename,
with the rationale recorded), **add** `list.remove`, and **defer** the
key-side/set combinator tail with reasons.

## Motivation

A learner cannot currently predict, across containers:

- membership: `list.contains` / `set.contains` / `string.contains` vs
  `dict.contains_key`;
- removal: `dict.remove` and `set.remove` exist; `list` has no
  remove-by-value at all — the idiom is
  `list.filter(xs, fn(y): y != x)`, which removes *all* occurrences and is
  the kind of thing a reader must stop and verify;
- transformation: `dict.map_values` exists with no key-side dual; `set` has
  no `map`/`filter`.

Pre-0.1 is the last cheap moment to decide these (break-don't-deprecate), and
recording the *rejections* matters as much as the changes — the membership
asymmetry in particular will otherwise be re-reported by every future audit.

## Design

### 1. `dict.contains_key` stays — decided, rename rejected

The audit proposed renaming to `contains` for cross-module uniformity.
Rejected: a dict holds *pairs*, so bare `contains` is genuinely ambiguous —
membership of a **key** or of a **value**? `list`/`set`/`string` have no such
ambiguity (one element domain), so bare `contains` is right there and the
asymmetry is **meaningful, not sloppy**. Rust made the same call
(`HashMap::contains_key` beside `Vec::contains`), and the readable-names rule
(explicit over terse) points the same way. The name is kept, and this section
is the citable record.

One doc-comment addition to `std/dict.witchy`'s `contains_key`: state that the
`_key` suffix is deliberate disambiguation, so the generated reference carries
the rationale.

### 2. `list.remove` — added

```witchy
// A new list with the FIRST occurrence of `target` removed; unchanged when
// absent. Removing every occurrence is `list.filter(xs, fn(y): y != target)`.
pub fn remove(var xs: List(a), target: a) -> List(a) where a: Eq:
```

- **First occurrence only**, mirroring `dict.remove`/`set.remove` (which
  remove "the" entry — at most one exists there; "at most one removed" is the
  common contract).
- Absent target → unchanged input, exactly like `dict.remove`
  (`std/dict.witchy:36`: "unchanged when absent") — RFC-0044's lookup-miss-
  is-not-an-error rule.
- `var` receiver so the statement form (`xs.remove(v)`) writes back like its
  dict/set siblings (RFC-0022/0043).
- `Eq` bound, matching `list.contains` (`std/list.witchy:454`).
- Doc-comment written to render correctly in the generated `spec/stdlib.md`
  (regenerate with `witchy doc std/*.witchy > spec/stdlib.md`; never
  hand-edit).
- Both backends get it for free (pure witchy), but per house rules it still
  ships with a differential test in `src/example_tests.rs` (present / absent
  / duplicate-element cases) and a line in the book's collections material if
  one lists the removal family.

### 3. Deferred, with reasons (the record against re-audit)

- **`set.map` / `set.filter`** — deferred. `set.filter` is uncontroversial
  but low-demand (zero call sites in `projects/**` want it today);
  `set.map` silently changes cardinality under non-injective functions —
  a semantic worth deciding only when a real consumer exists. The
  `to_list`/`from_list` round-trip is the documented idiom meanwhile.
- **`dict.map_keys` / `dict.filter_keys`** — deferred, same collision
  question (non-injective key maps merge entries — which value wins?). 
  `dict.pairs` → transform → `from_pairs` is the idiom; `from_pairs`'s
  last-wins behavior is at least explicit at the call site.
- **`bytes` enrichment** (search/split/reverse) — rejected for 0.1. `Bytes`
  is deliberately the minimal flat binary payload (`std/bytes.witchy` header);
  crypto consumers argue for a small, auditable surface. The `to_list`/
  `to_string` bridges are the documented escape.
- **Access-failure uniformity** (trap `at` / Option `get` / clamping
  `substring`) — **already settled** by RFC-0044 rule 5 (names are the
  contract; documented clamping is legal). Not reopened; recorded here so the
  trio stops resurfacing as a finding.
- **`split_once` sentinel** — owned by BUG-508 (RFC-0044 policy applies);
  not duplicated here.

### Verification

- `./scripts/check.sh --fast` while iterating; full gate via
  `./scripts/merge-queue.sh submit <branch>`.
- `witchy fmt` over `std/` (the only formatting gate for witchy code); never
  `cargo fmt`.
- Regenerated `spec/stdlib.md` must pass `stdlib_docs_are_current`.
- Differential test for `list.remove` on both backends; `witchy parity` on the
  book example if one is added.

## Alternatives

- **Rename `contains_key` → `contains`** — rejected above; ambiguity beats
  uniformity here, and Rust's precedent is the right prior.
- **`list.remove` removes all occurrences** — rejected: diverges from the
  dict/set "at most one" contract and duplicates `filter`'s one-liner.
- **`list.remove` returns `Option(List(a))`** (None when absent) — rejected:
  no container removal here signals absence; callers that care test
  `contains` first, same as dict.

## Drawbacks

- One more `Eq`-bounded list function to maintain across both backends'
  protocol machinery (post-RFC-0046 this is routine).
- Deferring the set/dict combinator tail means the `to_list` round-trip idiom
  stays slightly clunky; accepted until demand shows up in real code.

## Prior art

- Rust: `Vec::contains` / `HashMap::contains_key` (the ambiguity argument),
  `Vec::remove` (by index) — witchy's by-value `remove` follows its own
  dict/set precedent instead, which is the more consistent local choice.
- Go: no generic container methods at all — the gap witchy's stdlib exists to
  close (Go-parity direction).
