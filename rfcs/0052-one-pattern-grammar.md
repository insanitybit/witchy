---
rfc: 0052
title: "One pattern grammar; contexts differ only by refutability"
status: proposed
created: 2026-07-03
tracking:
---

# RFC-0052: One pattern grammar; contexts differ only by refutability

> Provisional syntax throughout. Code blocks are intentionally **not** tagged
> `witchy` so the doc-examples sweep does not compile pre-implementation
> snippets.

## Summary

Witchy has one `Pattern` AST but *four* pattern grammars, one per binding
context, each an accident of which parser function the context happens to
call. This RFC replaces them with **one grammar for every binding position**
— `match` arms, `if let`, `while let`, `let`, `for`, and comprehensions — with
exactly one per-context rule: **refutability**. Refutable contexts
(`match`/`if let`/`while let`) accept any pattern; irrefutable contexts
(`let`/`for`/comprehensions) accept any pattern the checker can prove always
matches, and reject the rest with an error that points to `if let`. Nested
or-patterns become real patterns; Float literal patterns are rejected with a
teaching error; Duration literal patterns are admitted; `@` bindings are
explicitly deferred.

## Motivation

The current fragmentation, fully re-probed against the shipped binary
(2026-07-03) — it is worse than "match vs the rest":

| Pattern | `match` arm | `if let`/`while let` | `let` | `for` | comprehension |
|---|---|---|---|---|---|
| flat tuple `(a, b)` | yes | yes | yes | yes | **no** (parse error) |
| nested tuple `((a,b),c)` | yes | yes | **no** | **no** | no |
| record `Point(x, y)` | yes | yes | **no** | no | no |
| list `[a, ..rest]` | yes | yes | **no** | no | no |
| or-pattern `1 \| 2` | yes (top-level arms only) | **no** (parse error) | — | — | — |
| range `0..10` | yes (undocumented) | **no** (parse error) | — | — | — |
| nested or `Some(1 \| 2)` | **no** (parse error) | no | — | — | — |
| Float literal `1.5` | **no** (`expected a pattern, found `1.5``) | no | — | — | — |
| Duration literal `1s` | **no** (`expected a pattern, found `1000ms``) | no | — | — | — |

Each row traces to a parser accident, not a decision
(crates/witchy-syntax/src/parser.rs):

- `match` arms go through `match_expr` (:1644) → `arm_pattern` (:1719) →
  `pattern` (:1762). Or-patterns and integer ranges live only in the first
  two — or-alternatives are expanded into duplicate arms at parse, ranges
  desugar to a fresh binding plus a synthesized guard — which is *why* they
  exist nowhere else and why `Some(1 | 2)` can't parse (inside `pattern()`,
  `|` is nothing).
- `if let`/`while let` call bare `pattern()` (:1598, :1243): full structural
  patterns, but no or/range, though nothing about those constructs is
  match-specific.
- `let` never calls `pattern()` at all — it hand-parses a flat
  identifier-tuple into `Stmt::LetTuple { names: Vec<String>, … }` (:930–948).
  `let ((a,b),c)`, `let [a,b]`, and `let Point(x,y)` are parse errors — even
  though a single-constructor record pattern is **irrefutable**, so
  refutability is demonstrably not the rationale; the rationale is that
  `LetTuple` holds `Vec<String>`.
- `for` hand-parses its own flat name-tuple (:1284–1296) and desugars to a
  `LetTuple` in the body — so it inherits `let`'s limits (nested tuples
  rejected).
- Comprehensions hand-parse a *single identifier* per generator
  (`list_comprehension`, :1505–1517) — rejecting the tuple pattern the
  equivalent `for` accepts, despite the spec teaching them as the same
  iteration form.
- `Pattern` (crates/witchy-syntax/src/ast.rs:499) has **no Float or Duration
  variant**, so those literals are unmatched everywhere; the Duration error
  names a token the user never wrote (`found `1000ms`` for a source `1s`),
  because the lexer normalizes duration literals to milliseconds before the
  parser ever sees them.
- Ranges *work* in match and are documented nowhere: spec/language.md §6
  (:306) lists "literals, `_`, variables, constructors with nested patterns,
  tuples, list shapes, and guards" — no ranges. Working-but-undocumented
  surface is spec debt cashed in every time someone reads the spec to learn
  what patterns exist.

Users cannot form a rule that predicts this table. "Patterns work in pattern
position" is the rule they'll bring from any parent language; every **no**
above is a paper cut against it.

## Design

### 1. One parse path

All six contexts parse patterns through a single entry point:

- `pattern()` becomes the full grammar: everything it has today **plus**
  or-alternatives at any depth (`Pattern::Or(Vec<Pattern>)`, a real AST node)
  and ranges (`Pattern::IntRange { lo, hi, inclusive }`) as ordinary
  sub-patterns — so `Some(1 | 2)`, `(0..10, _)`, and `[1 | 2, ..rest]` all
  parse.
- `arm_pattern`'s parse-time tricks are retired: or-patterns stop being
  duplicate-arm expansion (the checker handles binding-consistency across
  alternatives: each alternative must bind the same names at the same types —
  the current expansion got this for free and the new node must enforce it);
  ranges stop being synthesized guards (exhaustiveness can then reason about
  them as patterns; today's guard-desugar makes every range arm invisible to
  exhaustiveness, which is why a fully-covering range match still demands `_`).
- `let` and `for` replace their hand-rolled name-tuple parsing with
  `pattern()`. `Stmt::LetTuple { names }` generalizes to
  `Stmt::LetPattern { pattern }`; `for`'s header and the comprehension
  generator take the same node (comprehensions accept exactly what `for`
  accepts **by construction** — same parse call, and the comprehension
  desugar already emits a `for` loop, parser.rs:1505).

### 2. One per-context rule: refutability

The checker (not the parser) classifies every pattern:

**Irrefutable**: `_`; a variable; a tuple of irrefutable patterns (any
nesting); a record pattern (single-variant type) whose fields are irrefutable;
a constructor pattern for a **single-variant** enum with irrefutable fields.

**Refutable**: literals (Int/String/Bool/Duration); ranges; or-patterns
(even if the alternatives happen to cover — keep the classifier simple and
predictable; a provably-total or-pattern in `let` is not a use case worth the
rule complexity); list patterns of any shape (`[]`, `[a, b]`, `[a, ..rest]`
— a list's length is never statically known); constructor patterns for
multi-variant types.

Rules:

- `match` / `if let` / `while let`: any pattern. (An *irrefutable* `if let` —
  today silently fine — gets the existing unreachable-arm treatment: the
  wildcard else-arm is dead, and the checker already rejects dead arms.)
- `let` / `for` / comprehensions: irrefutable only. The rejection teaches:

```
error: `let Circle(r) = shape` — `Circle` is one of 2 variants of `Shape`,
       so this pattern can fail. Use `if let Circle(r) = shape:` (with an
       else), or `match`.
```

This unlocks, immediately and by one mechanism: `let (a, (b, c)) = …`,
`let Point(x, y) = p`, `for ((k, v), i) in …`, `[a + b for (a, b) in pairs]`,
and destructuring a single-variant wrapper enum in `let`.

### 3. The literal edges, decided

- **Float literal patterns: rejected, with a teaching error** (not a silent
  grammar hole): ``Float literals cannot be matched — exact Float equality is
  a precision trap; bind and guard instead (`x if math.float_abs(x - 1.5) <
  eps ->`)``.
  Rust deprecated float literal patterns (future-incompat since RFC 1445;
  the structural-match saga) for exactly this reason; witchy, with NaN
  already banned from ordering and (per [RFC-0047](0047-one-equality.md))
  Float banned from Eq, should not open the door Rust is trying to close.
  Match-binding a Float scrutinee (`match 1.5: x -> …`) must of course work —
  today it check-passes then **fails at codegen** ("cannot compile to WASM…
  interpreter-only feature?"), a check-passes-codegen-fails hole this RFC
  closes as part of the pattern-lowering work.
- **Duration literal patterns: allowed.** `Duration` is an exact `i64` of
  milliseconds — no float hazard — and durations are already first-class
  literals with `==`. `match d: 1s -> …` parses to `Pattern::Duration(i64)`
  and the error that names `1000ms` for a source `1s` dies with the gap.
  Related lexer gap, fixed alongside: `-1s` does not parse as a negative
  duration literal today (`let d: Duration = -1s` is `expected Duration,
  found Int` — the sign never folds into the literal, unlike integer
  patterns, which `pattern()` special-cases at parser.rs:1809). The lexer
  folds a leading `-` into duration literals the same way patterns fold it
  into ints, in both expression and pattern position.
- **Ranges: documented, and slightly widened.** `lo..hi` / `lo..=hi` become
  real patterns (§1) usable at any depth in refutable contexts. Spec §6 gains
  the row it has been missing since ranges shipped. Range patterns stay
  Int-only (Duration ranges are conceivable; not needed, not now).

### 4. Explicit non-goals

- **`@` bindings** (`n @ 1..10`) — deferred, not designed. Nothing in std or
  the corpora wants them yet; the grammar stays small until they earn a slot.
  Range patterns becoming real AST nodes (rather than guard-desugars) keeps
  the door open without pre-committing syntax.
- **String/prefix patterns, exhaustive integer-range checking** — out of
  scope; exhaustiveness over ranges keeps requiring a final `_`/binding arm
  (the classifier treats ranges as refutable, full stop).
- **Guard placement** is unchanged: guards belong to match arms
  (`PAT if cond ->`), not to patterns.

### 5. Where the code changes (design level)

- **AST**: `Pattern::Or`, `Pattern::IntRange`, `Pattern::Duration` added;
  `Stmt::LetTuple` → `Stmt::LetPattern`; `for`/comprehension headers carry a
  `Pattern`.
- **Parser**: one grammar in `pattern()`; `arm_pattern`'s or/range handling
  moves in; `let`/`for`/comprehension call it. Net deletion of three
  hand-rolled sub-parsers.
- **Checker** (crates/witchy-types/src/typeck.rs): `check_pattern` learns the
  three new nodes; the refutability classifier is new (~the same shape as the
  existing exhaustiveness machinery at :2895–2960, which already knows
  variant counts via `adt_variants`); or-pattern binding-consistency is new.
  Exhaustiveness gains or/range nodes (or-patterns: union of alternatives'
  coverage; ranges: still no numeric-coverage analysis — see non-goals).
- **Both backends**: pattern lowering gains the three nodes. Or-patterns can
  lower exactly as today's parse-time expansion did (N arms), now placed in
  lowering where both backends share it via the linked module — parity by
  construction. The interpreter's matcher adds the same three cases. Every
  new form gets differential tests; the `let`-rejection errors get
  message-pinned type-error tests (the existing 79-test pattern).
- **Spec**: §6 rewritten around the grammar + refutability table above —
  which also pays the standing documentation debt (ranges), and §5's `let`
  section documents destructuring.

Interaction: [RFC-0042](0042-module-namespaces.md) adds qualified constructor
patterns (`iter.Item(x, _)`) — orthogonal (a name-resolution change, not a
grammar change); whichever lands second rebases trivially.
RFC-0043 (declared mutation write-back)'s `for var` keeps its
single-plain-variable restriction (it's a write-back form, not a pattern
context).

## Alternatives

- **Extend `let` to "irrefutable-looking" shapes only (nested tuples +
  records), keep separate parsers.** The minimal patch. Rejected: it keeps
  four grammars and just moves rows around the table; the next construct
  (say, `let [a, b] = pair_list` when lengths are typed someday) re-opens the
  same negotiation. One parse path is *less* code and a rule users can state.
- **Make `let` refutable with an implicit abort** (Python's unpacking
  ValueError; `let Some(x) = o` traps on None). Rejected: witchy routes
  "might not match" through `Option`/`Result`/`if let` everywhere else;
  adding an abort-on-mismatch binding form would be a second, worse error
  story beside `?`. The teaching error pointing at `if let` *is* the design.
- **Allow Float literal patterns (Go/C switch tradition).** Rejected: exact
  Float equality in a pattern is a bug generator (Rust's deprecation is the
  documented prior art), and it would contradict RFC-0047's Float-is-not-Eq
  posture.
- **Or-patterns stay parse-time arm expansion, just recursively.** Tempting
  (no new AST node), rejected: expansion is exponential in nested-or count,
  destroys arm identity for diagnostics ("arm 3 of 2 is unreachable"), and
  can't be reused by `if let`.
- **Do nothing.** The table in Motivation *is* the do-nothing outcome; it
  fails the language's own "one general mechanism" bar.

## Drawbacks

- **Wide but shallow surgery**: parser, AST, checker, two backends, spec —
  the cross-cutting kind of change where parity discipline matters most.
  Mitigated by the differential suite and by the fact that every currently-
  working program keeps working (the change is strictly additive on accepted
  programs, plus new rejections only for currently-*impossible* forms).
- One genuine new rejection class: an irrefutable-`if let` dead-else now
  errors (unreachable-arm rule) where today it silently runs — technically
  breaking for code that wrote a pointless `if let (a, b) = pair`.
- The refutability classifier is new semantic surface to document and hold
  stable; single-variant enums flipping to multi-variant now *also* break
  `let` destructuring sites (correctly — the pattern became fallible — but
  it's a new kind of downstream breakage to explain).
- Or-pattern binding-consistency checking (same names, same types, every
  alternative) is genuinely fiddly; Rust's implementation history says budget
  real test effort here.
- `-1s` lexer folding is a (tiny) breaking lex change: `x -1s` as a
  subtraction with no spaces around `-` re-lexes; `fmt`'s canonical spacing
  makes this a non-event in formatted code.

## Prior art

- **Rust** — the model: one pattern grammar, contexts split by refutability
  (`let` requires irrefutable, E0005 points you to `if let`/`let else`);
  or-patterns at any depth (stabilized 1.65 after the same
  expansion-vs-real-node debate resolved toward real nodes); float literal
  patterns deprecated via the structural-match RFCs — the direct precedent
  for §3.
- **Python** — a cautionary tale in both directions: assignment unpacking is
  refutable-with-runtime-error (rejected above), and PEP 634 `match` added a
  *second* pattern grammar late; starting unified is cheaper than merging
  after.
- **Swift** — `if case let` / pattern grammar shared between `switch` and
  conditional bindings; evidence the shared-grammar design carries to a
  layout-styled surface.
- spec/language.md §6 (:306) — the current documented grammar this RFC
  completes and corrects; [RFC-0028](0028-ergonomic-mutable-value-semantics.md)
  (`for var`, whose restrictions this RFC deliberately leaves in place).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields, and appending
    dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
