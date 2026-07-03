---
rfc: 0049
title: "The naming lexicon: conventions and the rename cut"
status: proposed
created: 2026-07-03
predecessors:
  - "feedback: readable names (spell out identifiers; terse only for synthetics)"
  - "0044 (std error policy — shape changes; this RFC owns the *name* changes)"
  - "scratch/consistency-analysis-2026-07-03.md §6 + the lexicon concordance"
tracking:
---

# RFC-0049: The naming lexicon — conventions and the rename cut

## Summary

std's naming has strong systems the audit could not find a violation of
(receiver-first: 0 violations in 559 signatures; `length` universal;
`from_*` 10/10) and sprawl at the edges (`_of` with four unrelated meanings,
four meanings of `count`, inverse encodings that read in opposite directions).
This RFC codifies the working systems as normative CONTRIBUTING.md rules and
executes one breaking rename cut for the defectors — names only; return-shape
changes are [RFC-0044](0044-std-error-policy.md)'s, and where a function gets
both (the `index_of` family) the two land in the same release cut with each
decision recorded in its own RFC.

## Motivation

A lexicon is only useful if it has no exceptions a user must memorize. Today:

- `count` means four things: lazy-stream consumption (`iter.count`), count-
  matching-a-predicate (`list.count`), count-substring-occurrences
  (`string.count`), count-equal-elements (`cmp.count`).
- `_of` means four things: json extraction (`int_of`), json construction
  (`value_of` — direction-inverted; it is a `from_value`), collision-dodging
  (`cmp.max_of`), plain accessor (`rights.rights_of`). And
  `encoding.base64url_of_hex` sits beside `base64url_to_hex` — inverse
  operations whose names read in opposite directions.
- `semver.lt` is the lone terse defector from the cmp trait's own spelled-out
  vocabulary (`less`, `less_equal` — the flagship of the readable-names rule).
- `dict.set_at` is a literal alias of `dict.insert` in the same module
  (`std/dict.witchy:18-25`) — two public names, one operation, kept only as
  the `d[k] = v` desugar target.
- `rand` (capability CSPRNG) vs `random` (pure seeded LCG): nothing in the
  names signals the security-relevant distinction — rated the worst-confusion
  module pair in the audit.

None of these is individually costly; collectively they mean the docs teach
conventions the API then breaks. Pre-1.0 with break-don't-deprecate in force,
this is the cheapest this cut will ever be.

## Design

### 1. Codify what already works (normative, verbatim into CONTRIBUTING.md)

These are *descriptions of the existing API* promoted to rules:

- **The lookup ladder**: `at` traps (partial), `get` returns Option (total),
  `get_or` takes a default, `require` aborts/Errs with a message. Any new
  container/lookup API uses these names with these semantics, no others.
- **`length`** is the one word for element count as a property. Never `len`,
  `size`, or `count`.
- **Directions**: `from_x` constructs from x; `to_x` converts to x; `as_x` is
  a cheap view/reinterpretation. A pair of inverses must read in one
  direction (`x_to_y` / `y_to_x`).
- **Predicates** are `is_*` or an established transitive verb
  (`contains`, `matches`, `starts_with`). Never tense- or number-only
  distinctions between two APIs.
- **Parameter names are API** (they render in the generated `spec/stdlib.md`):
  receivers `xs`/`d`/`s`/`it`, callbacks `f`/`pred`/`keep`/`less`/`thunk`,
  indices `i`/`index` per the taxonomy. New signatures draw from the taxonomy.
- **Abbreviations live in module names only** (`cmp`, `fs`, `vm` are fine);
  function names are spelled out (`first`/`second`, not `fst`/`snd`), with a
  whitelist for established math/crypto jargon (`sqrt`, `isqrt`, `pow`,
  `sha256`, `hmac`, `lerp`).
- **`_of` is banned for new API.** Existing uses are dispositioned below.

### 2. The rename cut

Maintainer-reviewed decisions (amending the reviewer's raw list):

| Current | Decision | Rationale |
|---|---|---|
| `iter.count` | **KEEP** | Rust's word deliberately signals *consuming the stream* (it drives to exhaustion — `std/iter.witchy:305-307`); `length` would imply a cheap property. The lexicon gains a note: on `Iter`, `count` = consume-and-tally. |
| `list.count(xs, pred)` | → `count_where` | Frees `count` from the predicate meaning on eager containers; the name states the signature. |
| `string.count(s, sub)` | **KEEP** | Counting substring occurrences is the established reading (Python `str.count`); no predicate confusion. |
| `dict.set_at` | **DELETE**; `d[k] = v` desugar retargeted to `insert` | Dict keeps `insert` (matching `set.insert`). The parser's place-assign desugar (`crates/witchy-syntax/src/parser.rs:1907-1920`) emits `set_at` today for *both* list and dict via UFCS; it becomes type-directed post-typeck (`insert` for Dict, `set_at` for List) — a small piece of [RFC-0043](0043-declared-mutation-writeback.md)/[RFC-0050](0050-method-call-generalization.md)'s receiver-type-aware direction. |
| `semver.lt` | → `less` | Aligns with the cmp trait's spelled-out vocabulary; callers: pm/coven resolution code, one cut. |
| `rights.covered` | → `any_covers` | `covered(declared, demanded)` reads backwards (what is covered?); `any_covers` states the quantifier and the direction. |
| `json.value_of` | → `from_value` | The only direction-inverted `_of` (it *constructs* Json from a value). Callers include glamour examples — wide but mechanical. |
| `csv.parse` / `csv.parse_records` | → `decode` / `decode_records` | Serialization formats decode, paired with `encode` (which csv already has); aligns with json/toml. The Result-shape change rides in RFC-0044's same cut. |
| `encoding.base64url_of_hex` | → `hex_to_base64url` | Now reads as `base64url_to_hex`'s inverse, in the same direction. jwt/webauthn callers updated in-cut. |
| json extraction `int_of`/`string_of`/… | **KEEP for this cut** | `_of` is banned for *new* API; churning the whole json accessor surface exceeds this RFC's blast-radius budget and json self-consistently uses it. Revisit only if [RFC-0054](0054-structured-errors.md) reshapes them anyway. |
| `cmp.max_of`/`min_of`, `maximum`/`minimum` | **KEEP, documented** | The min/max sextet's names are collision-driven (flat function namespace within a module family); a satisfying fix is overload-by-module, which is [RFC-0042](0042-module-namespaces.md)'s territory, not a rename. |

**The find-index grid** (by-value/by-predicate × sentinel/Option) resolves as
— names recorded here, shapes in RFC-0044:

- `index_of` family (`string.index_of`, `string.last_index_of`,
  `list.index_of`) — **names stay**, return shape becomes `Option(Int)`
  (RFC-0044 rule 1).
- `list.find_index` — **deleted** (it was the sentinel-shaped by-predicate
  cell).
- `list.position` — **stays** as the by-predicate Option-index form; it
  gains the by-predicate role `find_index` vacates. (Today `position` is
  by-value — `std/list.witchy:281` — so its argument changes to a predicate;
  by-value search is `index_of`. One name per axis, both Option.)

**Doc-only alignments** (param names are API): `option.filter`'s callback
renames `pred` → `keep` to match `list.filter`/`iter.filter`
(`std/option.witchy:44` vs `list.witchy:96`/`iter.witchy:94`); `list.at`'s
`index` vs `list.get`'s `i` align on `index`. Regenerate `spec/stdlib.md`
from the edited doc-comments; never hand-edit it.

### 3. `rand` vs `random` — decided: `random` → `prng`

Recommendation adopted: rename the pure seeded module `random` → `prng`, keep
`rand` as the capability-backed CSPRNG module. Weighing it honestly:

- The confusion is asymmetric and security-relevant: reaching for `random`
  when you meant `rand` hands you a Park–Miller LCG for a token. The module's
  own header shouts "NOT for cryptography" — a name that says *prng* makes the
  header redundant, which is what a good name does.
- Breakage is minimal — measured: exactly **one** in-tree importer of
  `random` (`examples/dice`), versus `rand`'s several. This is the
  worst-confusion-per-lowest-breakage rename available anywhere in the tree.
- The alternative (fold both behind one module with explicit seeded
  constructors) was rejected: the two halves have different *capability
  footprints* (Rand is a grant; prng is pure), and merging them would blur
  exactly the line the capability audit needs sharp — a pure module must stay
  visibly pure.

### 4. Ordering constraints

- **The `cmp.member`/`cmp.index_of`/`cmp.count`/`cmp.unique` quadruplet is
  NOT touched by this cut.** Its deletion is blocked on
  [RFC-0046](0046-typed-trait-dispatch.md): the Eq-bounded `list.*` forms
  cannot exist until dispatch consumes the real TypeTable (today
  `list.unique` fails to compile for record types on WASM — the documented
  reason cmp.* survives). Renaming names that are scheduled for deletion is
  double churn; they are frozen until 0046 lands, then deleted, not renamed.
- The `dict.set_at` deletion requires the desugar retarget in the same
  commit (the sugar must never dangle).
- RFC-0044's shape changes and this RFC's renames to the *same* functions
  (`index_of` family, `csv.parse`) land in one release cut so callers are
  touched once.

### 5. The three CONTRIBUTING.md rules (final form, verbatim-adoptable)

> **Naming 1 — one verb per concept.** Look up: `at`/`get`/`get_or`/`require`
> (trap/Option/default/abort — pick by contract, never invent a fifth).
> Convert: `from_x` in, `to_x` out, `as_x` view; inverses read in one
> direction. Serialization: `decode`/`encode`; human-text analysis: `parse`.
> `_of` is banned in new API. Removal is `remove`; element count is `length`.
>
> **Naming 2 — predicates and parameters.** A boolean function is `is_*` or an
> established transitive verb (`contains`, `matches`, `starts_with`); never
> distinguish two APIs by tense or number alone. Parameter names are public
> API (they appear in the generated stdlib reference): receivers `xs`/`d`/`s`/
> `it`, callbacks `f`/`pred`/`keep`/`less`/`thunk`, indices `index` —
> draw from the taxonomy, don't coin.
>
> **Naming 3 — abbreviate modules, spell out functions.** Module names may be
> short (`cmp`, `fs`, `vm`); function and type names are spelled out
> (`less_equal`, not `le`), except whitelisted math/crypto jargon (`sqrt`,
> `pow`, `sha256`, `hmac`). Modules whose public type names are
> collision-prone in the flat type namespace self-prefix them (as `json` does
> with `JsonInt`/`JsonNull`) until [RFC-0042](0042-module-namespaces.md)
> makes prefixes unnecessary.

## Alternatives

- **Do nothing.** The systems that work stay uncodified (so they erode — the
  `semver.lt` and `option.filter pred` defections are recent) and the
  four-meanings-of-`count` tax compounds with every module. Rejected.
- **Aliases / deprecation cycle.** Directly violates break-don't-deprecate;
  every alias is a second name the lexicon must then explain. Rejected.
- **Rename `iter.count` → `iter.length`** (the reviewer's original). Rejected
  by maintainer review: `count` on a lazy stream is Rust's deliberate signal
  that the operation consumes; `length` would falsely advertise a property.
- **Merge rand+random into one module.** Rejected above — capability-footprint
  clarity beats API unity here.
- **A documented decision not to rename `random`** was on the table (the brief
  allows it); rejected because the measured breakage (one example) removed
  the only argument for keeping the confusing name.

## Drawbacks

- ~20 in-tree files touch `index_of`/`find_index` and 6 touch `set_at`; the
  glamour examples use `value_of` widely. The suite, executed book fences,
  and `witchy fmt` are the migration vehicle, but this is a real, wide,
  mechanical diff that will dominate one commit's review.
- `list.position` changing *argument* (value → predicate) is the sharpest
  edge in the cut: old call sites keep compiling only if the value happens to
  be a function (essentially never), so the type error catches it — but the
  error will not say "position changed roles"; a release-note entry must.
- Codified rules are a review burden and will occasionally fight a genuinely
  better local name; the whitelist mechanism is the pressure valve.
- The `_of` json carve-out leaves a documented inconsistency in place —
  accepted as scope control, honestly recorded in the lexicon table.

## Prior art

- Go's `gofmt`-grade convention discipline and Rust's API guidelines
  (RFC 199 / C-CONV: `as_`/`to_`/`into_` directions) — the direction rules
  here are the same idea with witchy's spelled-out-names twist.
- Python's `str.count` — the precedent for keeping `string.count`.
- The audit's finding that witchy's *system-level* conventions already beat
  both parents in spots (receiver-first: zero violations) — this RFC exists
  to keep that true at the edges.
