---
rfc: 0078
title: "Anonymous tagged unions and the structural tier"
status: proposed
created: 2026-07-08
tracking: design-first. Gated on WORK, not on the release number — the two
  gates are (1) BUG-562/BUG-563 fixed (filed during design probing) and
  (2) RFC-0054's std trust-boundary migration landed, so the named error
  model is the established default before this tier arrives. If both close
  before 0.1 ships, this may land in 0.1. Every empirical claim in this RFC
  was probed live on 2026-07-08 master.
related:
  - "0054 (structured errors — the NAMED error model; §Layering is normative)"
  - "0067 story 3 (one protocol per concept — why this is a tier, not a rival)"
  - "0005 stage 4 (caps in aggregates reject-first — the exclusion this reuses)"
  - "0046 (typed dispatch — injections ride its expected-type/obligation seams)"
  - "0052 (one pattern grammar — record patterns deferred to stay inside it)"
  - "0069 (structured TypeInfo — gains a Union case; coordinate)"
---

# RFC-0078: Anonymous tagged unions and the structural tier

## Summary

Witchy has anonymous *products* (`.{x: 1, y: 2}`) and they earned their keep.
This RFC adds the missing dual — anonymous *sums*, spelled directly in a
signature with no declaration — and, in doing so, completes and regularizes
the structural tier the language already half-has:

```witchy
fn parse(s: String) -> Result(Config, .[BadPort(Int) | MissingKey(String)]):
    ...
    Err(.BadPort(70000))

fn load(dir: Dir, path: String)
        -> Result(Config, .[NotFound | BadPort(Int) | MissingKey(String)]):
    let text = dir.read_opt(path) ?? return Err(.NotFound)
    parse(text)?        # widens into the larger set — no From impl, no wrapping
```

These are **tagged** unions — the variant name is the runtime tag — so both
backends represent them exactly like a declared enum (tag word + payload).
The untagged TypeScript form (`Int | Float`) remains impossible and rejected:
compiled values are bare slots with no runtime type, so discrimination is
unimplementable without a tag, and with a tag it *is* this proposal.

Everything below is decided; there is no open-questions section. Where a
question existed during design, its resolution is recorded inline with the
reasoning.

## Motivation

The #1 use is error unions. Combining two libraries' failures today forces a
choice between a named enum + `From` impls (RFC-0054's model — right for
public contracts, ceremony-heavy for a ten-line helper), `Result(T, String)`
(stringly, un-matchable), or `Option`-as-error (inverted polarity, banned by
RFC-0071). Anonymous unions add the tier the dogfood keeps reaching for:
*enumerate the failures in the signature, widen silently along `?`*.

Roc made open tag unions its entire error model and they are the
most-praised part of that language; OCaml's polymorphic variants have served
the role for two decades. The feature also serves non-error plumbing — a
worker's message set, a parser's token-or-trivia — anywhere a small closed
alternative is local to a few functions and naming it is pure tax.

A second motivation surfaced while probing: the structural tier is currently
**irregular** — tuples are denotable in signatures but anonymous records are
not; aliases can name tuples but not records; the pieces don't rhyme. §The
structural tier makes one rulebook out of it.

## The structural tier: probed inventory and the unifying rule

Witchy already has a structural kingdom. Its state on current master (every
line probed live, 2026-07-08):

**Works today:**

- Tuples `(A, B)` and arrows `fn(A) -> B` are structural and denotable in
  signatures; a tuple can be a *field of a named type* (`inner: (Int, Int)`).
- Non-generic aliases resolve end-to-end and are transparent
  (`type P = (Int, Int)`; `fn f(p: P)`; a bare tuple flows in). Alias cycles
  are rejected with a clean link error.
- Anonymous records are structurally equal and **field-order-insensitive**
  (`.{x:1, y:"a"} == .{y:"a", x:1}` → `true`; the desugar keys on the sorted
  field set), unify across branches, nest, and sit in containers.

**Broken or missing (dispositions bind this RFC):**

| Probed fact | Disposition |
|---|---|
| Two modules each using any `.{…}` collide at link: `` type `__anon0` is defined more than once `` | **BUG-562 (HIGH), filed — hard prerequisite.** The fix (shape-keyed synthetic naming, below) is the same mechanism unions need for tag identity. |
| `type Pair(a) = (a, a)` parses but every use is `unknown type` | **BUG-563, filed.** Fix or formally reject before this RFC lands; this RFC assumes **fixed** (generic aliases resolve) and marks each dependent feature. |
| Aliases are module-local; `pub type` is a parse error; `lib.Id` fails | **Deferred, non-blocking** — see §Aliases: structural types cross module boundaries *by spelling*, so alias export is ergonomics, not mechanism. |
| `.{x: Int}` is not a type expression (param/field/alias positions all parse errors) | **In scope** — §Type positions. |
| No `.{…}` patterns; no *named-field* patterns anywhere (even `Account(name: n)` is a parse error — record patterns are positional-only) | **Record patterns deferred** — they would introduce named-field patterns to the whole grammar, which is RFC-0052's jurisdiction. Union patterns (the feature) are v1 core. |
| No spread on anon records (`.{y: 9, ..p}` parse error) | **In scope** — same desugar as named-record spread. |
| `"${p}"` on `.{x: 1}` prints `__anon0(1)` — the synthetic leaks | Target rendering specified here (§Protocols); the fix itself folds into the protocol-matrix work (RFC-0070 D9). |
| Anon records rejected as compiled dict keys | Follows the Eq-compound-key machinery (extended on master 2026-07-08), not this RFC. |
| `p.x` on unconstrained generic `a` → type error "requires a record, found `?`" | **Correct; keep.** This *is* the no-row-inference line, already enforced. |

**The unifying rule** — one sentence that also resolves the `type` keyword's
overload (`=` vs `:`):

> **`type X = …` names a shape and never mints a type; `type X: …` mints a
> nominal type with constructors** — sealable, impl-able, authority-capable.

The structural family is then complete and uniform — positional product
`(A, B)`, named product `.{a: A}`, sum `.[T | U]`, arrow `fn(A) -> B` — under
one shared rulebook: structural equality; exact shape (no width subtyping
**except** union-widening, §Semantics 2); no trait impls; no capabilities
inside (reject-first); transparent aliasing. The hardening path is a
one-character diff: `type Point = .{x: Int}` (shape) becomes
`type Point: x: Int` (nominal) the day the data needs invariants, impls, or
sealing.

Two lines deliberately not crossed — they are where the TypeScript slope
starts: **no width subtyping on records** (exact shape, always) and **no
impls on structural types even when alias-named** (behavior attaches to
nominal types only; two independently-declared `Point` aliases must never
collide on an impl).

## Syntax

### Type position: `.[Tag | Tag(Payload, …)]`

```witchy
.[NotFound | BadPort(Int) | MissingKey(String)]
```

- `.[` becomes its own lexer token (`DotLBracket`), exactly as `.{` is
  (`DotLBrace`) — verified free: `[` cannot begin a type, `|` cannot appear
  inside a type paren, and there are no leading-dot float literals.
- **Why brackets:** brackets are already witchy's *tag-set* delimiter — the
  only bracket-in-type today is capability rights (`Dir[Read,Write]`), a set
  of tags refining what stands before them. `.[…]` extends the reading:
  brackets enclose a set of tags; the prefix says whose (`Dir`'s tags are
  rights, `.`'s tags are anonymous variants). Roc spells tag unions with
  brackets. The rejected `.(A | B)` had a superficially cute product/sum
  duality with `(A, B)`, but the duality is false here — witchy's anonymous
  product is `.{…}`, not `.(…)`, so dot-paren rhymes with nothing.
- **Why `|`, not comma:** rights-brackets are conjunctive (hold Read AND
  Write); a union is disjunctive. Witchy already spells disjunction `|` in
  or-patterns (`1 | 2 | 3`). `.[A, B]` would read "holds both".
- Tags are **uppercase-initial identifiers** (constructor-shaped); `.foo` in
  a union is a parse error. Payloads are positional types, arity ≥ 1 when
  parenthesized; a bare tag is nullary. Duplicate tag names in one union are
  a parse-time error.
- The dot prefix is mandatory: bare `[…]` in type position stays free for a
  possible future, and the anonymous-tier marker (`.` = "structural,
  undeclared") stays uniform with `.{…}`.

### Record type position: `.{field: Type, …}`

`.{x: Int, y: Int}` becomes a type expression valid wherever a type is
(params, returns, fields of named types, alias right-hand sides, generic
arguments). It denotes the shape-keyed synthetic record (§Representation), so
it costs the checker nothing new — the synthetic generic record types already
exist; the type expression names an instantiation.

### Value position: injection is `.Tag(payload)` at expression start

```witchy
Err(.NotFound)
Err(.BadPort(70000))
let step: .[Advance(Int) | Halt] = .Advance(2)
```

- The leading dot is load-bearing: bare `NotFound` continues to resolve to a
  *declared* constructor in scope (unchanged), so importing a module that
  declares `NotFound` can never silently change an injection's meaning.
  `.Tag` says "the anonymous tag of this name, typed by context" — the same
  disambiguation `.{` performs for records, and the same shape as Swift's
  implicit-member syntax.
- **Ambiguity, resolved.** Probed: continuation lines beginning with `.`
  parse as method chains today (`xs␤    .get_or("a", 0)` — ok), and method
  names may legally be uppercase (`fn Get(self)` — probed ok), so case cannot
  disambiguate. The rule: `.Tag` is legal **only where an expression starts**
  (statement start, after `=`, after `(`/`,`/`[`, after `->`, after
  `return`/`Err(`-style call openings). On a chain-continuation line, a
  leading `.name` is *always* a method chain — existing programs keep their
  meaning. To inject mid-chain, parenthesize: `(.Advance(2))`. This is a
  purely positional rule the parser applies with no lookahead.
- `.foo(…)` (lowercase) at expression start remains an error ("anonymous
  tags are uppercase") rather than being claimed for anything — reserved.

### Pattern position

```witchy
match load(dir, path):
    Ok(cfg)                 -> use(cfg)
    Err(.NotFound | .Gone)  -> retry()
    Err(.BadPort(p))        -> report_port(p)
    Err(.MissingKey(k))     -> report_key(k)
```

`.Tag` / `.Tag(subpatterns…)` are patterns; payload subpatterns are
positional, mirroring declared-variant patterns. They compose with the
existing grammar with **no new rules**: or-patterns apply (same
bind-same-names-at-same-types check), guards don't count toward
exhaustiveness (existing rule), and a union pattern is refutable unless the
union has one tag (so `let .Only(x) = e` obeys the existing
irrefutability check). Patterns check against the scrutinee's union type via
the existing `check_pattern(pat, expected)` entry point — the expected type
names the tag set, so tag lookup is closed and typo-diagnosable ("union has
no tag `.NotFuond` — did you mean `.NotFound`?").

## Semantics — all decided

1. **Closed, signature-spelled sets; no row inference.** A union type is
   always written in full where it appears. There are no open unions, no row
   variables, and no inference of a union from its injections: `let e =
   .NotFound` with no expected type is a check-time error ("annotate the
   union type"). Rationale: OCaml's polymorphic-variant diagnostics are
   notorious *because* of row inference; the closed-set rule buys ~all of the
   ergonomics at ~none of the cost. The no-row line already exists in the
   checker (probed: field access on unconstrained generics is rejected).

2. **Width subtyping, unions only, three sites.** `.[A | B]` is accepted
   where `.[A | B | C]` is expected — subset of tags, payloads unified
   pairwise — at exactly: argument passing, `return`/tail position, and `?`
   propagation. Nothing narrows implicitly; narrowing is `match`.
   *Implementation seam:* this rides `coerce_arg`
   (`crates/witchy-types/src/typeck.rs:3442`), the existing directed-coercion
   point where capability-rights subset coercion already lives — union subset
   coercion is the same shape (check subset, else fall through to `unify`).
   Records get **no** width subtyping, ever.

3. **Tag identity = name + payload type list, program-global.**
   `.NotFound` in module A and `.NotFound` in module B are the same tag —
   that is what makes widening structural and lets unions cross module
   boundaries by spelling alone. `.Conflict(Int)` and `.Conflict(String)`
   are simply different tags that share a spelling; both may exist in a
   program, but one union mentioning both is a check-time error (ambiguous
   tag within a set).

4. **Injection typing.** `.Tag(args)` infers as a fresh type variable
   carrying a deferred obligation *(tag name, payload types)*. The
   obligation resolves when unification pins the variable to a concrete
   union type containing that tag (payloads unify); a variable still
   unresolved at fixpoint end is the "annotate the union type" error.
   *Implementation seam:* this is the same deferred-obligation shape as
   bounded generic call obligations, which just landed
   (`typeck: enforce bounded generic call obligations`, 998bd4f1) — the
   machinery exists; unions add one obligation kind. In practice the
   expected type is almost always immediate: `Err(…)`'s argument type is
   fixed by the function's declared `Result`, annotations fix `let`, and
   argument positions fix calls.

5. **`?` and the context form.** Inside a function returning
   `Result(U, .[A | B | C])`, `e?` on `Result(T, .[A | B])` propagates via
   rule 2 — no `From`, no wrapper. **Decided:** the context form `e? "msg"`
   is a check-time error on union-error Results in v1 ("context messages
   attach to `String` errors; add a tag that carries your context, or
   `match` and rewrap"). Auto-wrapping into a synthesized `.Context(String)`
   tag was considered and rejected as magic: it would change the union's tag
   set at a distance. Mixed propagation (union error into a *named*-enum
   error) goes through RFC-0054's `From` as usual — a named enum can declare
   `From(.[…])` if its author wants to absorb a specific union; nothing
   special is synthesized.

6. **Generic payloads: allowed.** A payload may mention in-scope type
   parameters (`fn first(xs: List(a)) -> .[Found(a) | Empty]`). They
   monomorphize away like every other generic; tag interning happens
   **post-mono** where all payloads are concrete (§Representation). If
   implementation surfaces dispatch complications in the RFC-0046 fixpoint,
   the recorded fallback cut is concrete-payloads-only for v1 — a
   restriction, not a redesign.

7. **Protocols.** Unions get synthesized structural `Show`, `Reflect`, and
   `PartialEq` (per-tag, payloads structural). Rendering: `.NotFound`,
   `.BadPort(70000)` — dot included, marking the anonymous tier; likewise
   the record-render target is `.{x: 1}` (today it leaks `__anon0(1)` —
   fixed under RFC-0070 D9 to this spec). **No `Ord`** (no principled tag
   order). **No user impls** on any structural type — the moment behavior
   or an invariant is wanted, the answer is `:` (name it). `Eq`-bounded
   contexts (dict keys) follow the compound-key machinery, out of scope
   here.

8. **Exclusions (reject-first).** No capability types anywhere in a union
   payload or an anonymous-record field, at any depth — check-time
   rejection, same seam and same message family as RFC-0005 stage 4 (caps
   in aggregates). Authority stays strictly nominal: the structural tier
   can never carry, and therefore never launder, a capability. Function
   types in payloads are permitted (they are values), but then the union is
   uncomparable — exactly the existing `uncomparable_kind` rule for records
   with fn fields; the net already walks declared fields and will walk tag
   payloads.

9. **Nesting and composition.** Unions nest in payloads
   (`.[Wrapped(.[A | B]) | C]`), sit inside containers
   (`List(.[A | B])`), and may be fields of named types — anywhere a type
   goes. Channel message types remain out of scope for v1 (the
   one-message-type-per-program rule is RFC-0055 territory; a union there
   is attractive future work and is noted, not designed).

10. **Aliases.** `type LoadErr = .[NotFound | BadPort(Int)]` names the
    union locally (non-generic aliases resolve today — probed). Because
    identity is structural, an alias is *pure shorthand*: an importer who
    spells the same set has the same type, so **alias export is not needed
    for unions to cross modules** — this is the deep reason the tier works
    for cross-module plumbing without `pub` machinery. (`pub type` today is
    a parse error; exporting aliases is desirable ergonomics filed as
    follow-up, and generic aliases are blocked on BUG-563.)

## Representation & parity

Identical to a declared enum: a tag word plus payload slots — no new value
kind in either backend.

- **Interning.** Tag words are assigned once per program, post-mono, keyed by
  *(tag name, concrete payload type list)*. The linker/lowering sees the
  closed world, so the same key gets the same word everywhere — which makes
  **widening a runtime no-op**: `.[A|B]`'s bits are already valid
  `.[A|B|C]` bits. No re-tagging at coercion points. (Alternative
  considered: FNV name-hash tags à la codegen's `type_tag_of` — workable but
  collision-managed and sparser; interning is dense and collision-free.
  Decided: interning.)
- **Match lowering** compiles to the same tag-word dispatch as declared
  enums, both backends. The interpreter carries tags in its existing `Ctor`
  value shape; nothing new crosses the parity boundary.
- **The shape-keyed naming fix (BUG-562) is shared infrastructure**: anon
  records stop numbering per-module (`__anon0`, colliding at link) and key
  their synthetic type by sorted field set, program-wide; union tags key by
  the interning rule above. One mechanism, both structural kinds, and
  cross-module structural identity holds by construction.
- **Tests**: differential tests per feature leg (inject/match/widen/`?`;
  same-shape records in two modules; unions in containers), a `witchy
  parity` book example, and goldens (RFC-0072 harness) for the new
  diagnostics: annotate-the-union, ambiguous tag, unknown tag with
  did-you-mean, caps-in-payload rejection, `? "msg"`-on-union rejection,
  chain-vs-injection parse error.

## Layering with RFC-0054 — normative

RFC-0067 story 3 (one protocol per concept) is the constraint. The two error
models are **tiers with a boundary rule**, not rivals:

- **Named enums + `Error` trait + `From`** (RFC-0054) remain the model for
  every *public, cross-module contract*: std APIs, package boundaries,
  anything documented. Named errors carry impls, seal, appear in generated
  docs, and evolve deliberately.
- **Anonymous unions** are for *local plumbing*: within a module or a small
  call cluster, during iteration, before an error surface has settled. The
  lifecycle mirrors anonymous records': prototype with `.[…]`; when the
  union stabilizes or crosses an API boundary, harden it into a named enum —
  a mechanical transform (tags become variants; `=` becomes `:` in the
  alias case).
- **std never exports `.[…]` in a public signature.** That is a review rule
  (and a cheap lint later), recorded here so the tier cannot creep into the
  contract layer.
- One sentence ships in the book's error chapter: *"`.[…]` for errors that
  stay close to home; a named type the moment they travel."*

Shipping order follows from this: **after the 0054 std migration is the
established default** — landing both stories at once is exactly the
two-arriving-models incoherence RFC-0067 exists to prevent. The gate is that
ordering, not the release number: 0054's remaining scope is the
json/toml/grant/TUF/webauthn/pm trust-boundary migration (its own tracking
note), and the moment that cut lands, this RFC is unblocked.

## Prerequisites & sequencing

1. **BUG-562** (HIGH — cross-module `__anon0` collision): fix via shape-keyed
   synthetic naming. Independently worth doing *now*; it is a live defect in
   a shipped feature and this RFC's §Representation depends on it.
2. **BUG-563** (generic aliases dead on arrival): fix, or reject the
   parameterized-alias grammar; either resolves the accepted-but-unusable
   state. This RFC prefers **fix** (the tier's generic aliases —
   `type Tagged(a) = .{value: a, tag: String}` — depend on it) but survives
   rejection with that row scoped out.
3. **D9 render fix** (`__anon0(1)` → `.{x: 1}`) — protocol-matrix work,
   independent, target spelling specified here.
4. The RFC itself: after gates 1–3 close (work-gated, not release-gated);
   grammar + checker obligations + interning + tests in one cut
   (break-don't-deprecate needs no migration — nothing exists to migrate).

## Alternatives

- **Untagged unions (`Int | Float`)** — permanently rejected: bare-slot
  compiled values carry no runtime type; and unions over *capabilities*
  would make "what authority does this value hold" unanswerable from the
  type. With a tag added, it is this proposal.
- **`From`-only (status quo)** — right for contracts; the
  declaration-per-helper tax is what the dogfood's `Option(String)`-as-error
  disease (RFC-0071) looks like in practice.
- **Open unions / row inference (full OCaml)** — maximal power, notorious
  diagnostics; rejected for signature-spelled closed sets.
- **Bare tags (Roc-style `NotFound`)** — rejected: collides with declared
  constructors in scope; an import could silently re-bind an injection.
  The dot keeps anonymous and declared namespaces disjoint.
- **`.(A | B)` / bare `(A | B)` spellings** — rejected per §Syntax (false
  duality; wrong mental model).
- **Auto-`.Context(String)` wrapping for `e? "msg"`** — rejected as
  action-at-a-distance on the tag set (§Semantics 5).

## Drawbacks

- **A second error spelling exists at all.** Contained by the normative
  layering rule, the std review rule, and the after-0054 sequencing gate.
- **Signatures get longer** — the set is spelled at every hop. Deliberate
  (no row inference); module-local aliases recover brevity today, exportable
  aliases later.
- **The structural tier steps into type positions.** Deliberate and
  symmetric (§The structural tier): records and unions move together under
  one rulebook; the residual cost is two type-grammar productions and one
  new checker obligation kind.
- **Tag-identity-by-name means `.Timeout` unifies across modules.** For
  local plumbing that is the feature; where it isn't, name the type.
- **Record patterns don't ship with this** — field access covers records;
  named-field patterns arrive, if ever, through RFC-0052 for declared and
  anonymous records at once.

## Prior art

- **Roc** — tag unions as the error model (`[NotFound, BadPort I64]`,
  brackets): the closest shipped design and the evidence the ergonomics are
  worth it.
- **OCaml polymorphic variants** — two decades of the feature and the
  cautionary tale (row-inference diagnostics) the closed-set rule avoids.
- **Swift implicit members** (`.north`) — the value-position dot resolved
  by expected type.
- **witchy's own `.{…}`** — the tier being completed, and proof a
  second-class structural tier can coexist with a nominal core without
  eroding it.
