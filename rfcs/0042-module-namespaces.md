---
rfc: 0042
title: Module namespaces and Python-style imports
status: proposed
created: 2026-07-03
tracking:
---

# RFC-0042: Module namespaces and Python-style imports

> Provisional syntax throughout. Code blocks are intentionally **not** tagged
> `witchy` so the doc-examples sweep does not compile pre-implementation
> snippets.

## Summary

Functions are already module-scoped — `import iter` gives you `iter.map`, and
two modules exporting `map` coexist without a thought. Types are not: every
`type` in the whole link set lands in **one flat global namespace**, imported
unqualified (spec/language.md:821–824). This RFC makes types module-scoped
exactly like functions: `import iter` makes `iter.Step` a valid type name in
annotations, constructor patterns, and expressions; a new `from X import Y`
binds a name (type or function) unqualified, Python-style; two unqualified
bindings of the same name are a compile error at the import site. Enum variants
scope with their type — a `match` on a value of a known type resolves bare
variant names against that type, so match-heavy code keeps reading exactly as
it does today.

## Motivation

The flat type namespace is the single worst composition failure in the
language, and it is about to become ecosystem-fatal.

**Verified today (all probed against the shipped binary, 2026-07-03):**

- `import iter` + `import chan` **fails to compile**: both declare a `Step`
  type, and the error surfaces *inside std internals* —
  ``type error: `iter.map_step`, line 89: non-exhaustive match on `Step`:
  missing Done, Yield, Fork, Open, Push, Pull, PullAny, Wait, Cancel`` — the
  two variant sets have been silently merged into one type. Two flagship std
  modules are mutually exclusive, and the user is debugging a file they never
  wrote. `import task` + `import future` fails the same way (both declare
  `Step(m, a)` and `Task`; the error lands in `task.done`, line 57 — again
  inside std).
- There is **no qualified-type syntax to escape with**: `let s: iter.Step(Int)`
  is a parse error (`expected \`=\`, found \`.\``), and `iter.Item(1, …)` in
  expression position is ``link error: module `iter` has no function `Item` ``.
  The collision has no user-side workaround at all.
- `std/json` defensively self-prefixes every variant (`JsonNull`, `JsonInt`,
  `JsonArray`, …) — Hungarian notation that exists *only* because the namespace
  is flat, and that essentially no other module follows (chan's own types are
  bare `Step`/`Task`/`Slot` — which is exactly what collides).
- A user type can silently collide with a **built-in**: declaring
  `type Secret:` in your own module produces `expected \`Secret\`, found
  \`Secret\`` — two identically-rendered names in one error.
- With the coven registry ([package-manager.md](package-manager.md)), two
  third-party runes each defining a `Config` or an `Error` would be
  un-coimportable, and *neither author did anything wrong*. A convention can't
  fix this: we don't control external packages. This is the ecosystem-fatal
  case, and it is why the fix must land before packages proliferate.

The asymmetry is also just incoherent: the spec's own words are "everything
else says where it came from" — for functions. Types are the exception, and
the exception is the part that breaks.

## Design

### 1. Types become module-scoped, spelled like functions

A module's `pub` types (and their constructors) are reachable from an importing
module **only under the module qualifier**, exactly as its functions already
are:

```
import iter

fn main(console: Console):
    let s: iter.Step(Int) = iter.next(iter.from_list([1]))
    let one = iter.Item(1, iter.empty())     // qualified constructor, expression position
    match s:
        Item(x, _) -> print(console, "${x}")  // bare variant OK: scrutinee type is known (§4)
        Empty -> print(console, "empty")
```

Grammar changes:

- **Type position** (`Parser::ty`, crates/witchy-syntax/src/parser.rs:762): a
  type path is `ident ("." ident)? type_args?`. A lowercase first segment
  followed by `.` and a capitalized segment is a qualified type
  (`iter.Step(Int)`, `json.Json`). Today the `.` is a hard parse error, so the
  syntax is free.
- **Pattern position** (`Parser::pattern`, parser.rs:1762, the `Tok::Ident`
  arm): `mod.Ctor(pats…)` parses as a qualified constructor pattern.
- **Expression position**: `iter.Item(1, …)` currently resolves through the
  function-call path and dies at link; constructor resolution learns that a
  capitalized final segment on an imported module names that module's
  constructor.

`type` gains no new visibility syntax in this RFC: `TypeDef` has no `public`
flag today (unlike `Function`, ast.rs:154) and all types are effectively
exported. Making non-`pub` types genuinely private is compatible with this
design and left as a follow-up — the namespace fix must not wait on it.

### 2. `from X import Y` — explicit unqualified binding

```
from iter import Step, Iter
from json import Json

fn peek(s: Step(Int)) -> String: ...
```

- `from X import Y, Z` binds each listed name — a type or a function —
  unqualified in the importing module. It implies `import X` (the qualified
  names are also available).
- **`from X import *` does not exist.** Deny-by-omission is the house ethos:
  an unbounded import means a dependency bump can inject names into your scope
  and change what your identifiers mean. Every unqualified name is written down
  at the top of the file, so a reader can always answer "where did this type
  come from" without tooling. (Python's own style guides ban `import *` for the
  same reason; we just don't ship the footgun.)

### 3. Collision rule: loud, early, at the import site

Two unqualified bindings of the same name in one module — whether
from-imports, a from-import against a local declaration, or a from-import
against a prelude/built-in name — are a **compile error at the second import
line**, not at first use:

```
error: `from chan import Step` collides with `from iter import Step` (line 1)
       — both bind `Step` unqualified. Drop one and use the qualified name
       (`chan.Step` / `iter.Step`), or import under different names.
```

Qualified access always works; a collision is therefore never a dead end.
Plain `import X` binds *no* unqualified type names, so `import iter` +
`import chan` simply compiles — the headline failure dissolves without either
module changing.

We considered erroring only when the ambiguous name is *used*. Rejected: a
use-site error appears far from its cause, and an unused colliding import is
still a landmine for the next edit. Loud-and-early matches how the language
already treats duplicate top-level functions (typeck.rs:3097).

### 4. Variant scoping: variants travel with their type

The decision point: after `import iter`, how do you spell `Item`?

- **In a pattern whose scrutinee type is known**, bare variant names resolve
  against that type's variant set. The checker already threads the expected
  type into `check_pattern` and holds per-type variant lists
  (`adt_variants`, crates/witchy-types/src/typeck.rs:989, consulted for
  exhaustiveness at :2939) — resolution keys variants by their owning type
  instead of one global constructor table. `match s: Item(x, rest) -> …` keeps
  working verbatim. This is what keeps most existing code compiling (§6).
- **In expression (construction) position**, a bare variant resolves against,
  in order: the current module's own types, from-imported types (a
  from-imported type brings its variants' bare names with it), and the prelude
  (§5). Otherwise it must be qualified: `iter.Item(…)`, `json.JsonInt(1)`.

So variants scope *with their type*: `from iter import Step` makes `Item`/
`Empty` constructible bare; `import iter` alone requires `iter.Item`. This
matches how the checker already recovers types (the scrutinee tells you the
type; a bare construction has nothing to resolve against) and avoids a separate
per-variant import surface (`from iter import Step.Item` is rejected as
needless grammar).

### 5. The prelude stays ambient

`Option`/`Some`/`None`/`Result`/`Ok`/`Err` — and the primitive type names —
remain globally bare, no import required. They are load-bearing language
surface (`?`, `e? "msg"`, main-signature checking), not ordinary library types.
The prelude *modules* (`list`, `string`, `dict`, `math`, `option`, `result` —
linker.rs, the prelude pull-in) contribute exactly these type names ambiently
and nothing else. `cmp.Ordering` is deliberately **not** ambient: derive- and
comptime-generated code references it qualified, and a bare `Less` in your
match still resolves via the scrutinee rule (§4).

A user declaration that shadows an ambient name (`type Secret:` vs the built-in
capability) becomes an error under the §3 rule — fixing the
identically-rendered-names confusion above.

### 6. Where the implementation enters

The linker already does whole-program linking with per-module context
(crates/witchy-syntax/src/linker.rs); today functions get qualified as
`{module}.{name}` at merge (linker.rs:546) while types are pushed into the
merged module **untouched and untagged** (`Item::Type(t) =>
items.push(Item::Type(t.clone()))`, linker.rs:556) — that one line is where
the flat namespace is manufactured. The change, at a design level:

1. **Tag types with their module of origin at merge**: `Step` in module `iter`
   becomes canonical `iter.Step`; its constructors become `iter.Item`,
   `iter.Empty`. Same shape as function qualification, same place.
2. **Resolve per module, before merge.** A resolution pass rewrites each
   module's type references (annotations, constructor expressions, constructor
   patterns) to canonical names using that module's own declarations,
   `from`-imports, and the prelude — exactly where `aliases::resolve` and
   `check_sealing` already run per-module with the home module known
   (linker.rs, pre-merge). Unresolvable bare names in *pattern* position are
   left for the checker (next point); bare names in expression/annotation
   position that resolve to nothing are an error here, at the source line.
3. **Typeck resolves scrutinee-scoped variants.** `ctor_sigs` /
   `adt_variants` / `record_fields` (typeck.rs:976–989) key by canonical
   qualified names. `check_pattern` resolves a bare `Ctor` against the expected
   type's variant list; only if the expected type is unknown does it fall back
   to "is this bare name unambiguous across the link set" (with the ambiguity
   error naming the candidates).
4. **Trait dispatch follows.** `head_type_name` / `recover_generic_call`
   (crates/witchy-types/src/traits.rs:1035, :539) traffic in type-name strings;
   those strings become canonical qualified names. This RFC deliberately does
   not restructure that machinery — RFC-0046 (typed trait dispatch)
   does — but qualified names remove a whole class of its wrong-guesses (two
   `Step`s can no longer alias).
5. **Sealing is unchanged.** RFC-0002's link-time sealing already tracks the
   declaring module; canonical names make its job strictly easier.

### 7. Migration (one cut, break-don't-deprecate)

- **Programs that import a module and only call its functions and match on its
  types keep working unchanged** — function calls were already qualified, and
  §4 keeps bare variants in match arms. This is the majority of real usage
  (verified across examples/ and projects/: construction of imported types
  outside std is rare; consumption via match is the norm).
- **Construction sites and type annotations naming imported types** must add a
  qualifier or a `from`-import: `JsonInt(1)` → `json.JsonInt(1)` or
  `from json import Json` + bare variants. The suite (docs-as-tests plus the
  differential corpus) enumerates every such site mechanically; the compiler's
  unresolved-name error at each site names the module that exports the type.
- **std internally**: std modules importing each other's types (e.g. `iter`
  uses `Set` from `set`) adjust to qualified/from-import spellings in the same
  cut.
- **Follow-up, separate cut**: with the namespace fixed, `json`'s defensive
  prefixes can be dropped (`json.JsonInt` → `json.Int`), and the `Step`×3 /
  `Task`×2 / `Slot`×2 / `Handle`×2 name-clash cluster in chan/task/future/iter
  simply stops mattering. That de-prefixing rename belongs to
  [RFC-0049](0049-naming-lexicon.md), not here.

This RFC dissolves, by construction: the iter+chan block, the task+future
block, the user-type-vs-builtin shadowing trap, and the reason the Json*
convention exists. It also unblocks RFC-0055 (channel message types)
(generators and channels in one module) and gives
[RFC-0044](0044-std-error-policy.md)/[RFC-0054](0054-structured-errors.md) room
to introduce per-module error types without global name games.

## Alternatives

- **Mandated self-prefixing convention** (every module prefixes its pub types,
  as json does). Rejected: unenforceable on external packages — the one case
  that matters most — and it's Hungarian notation forever, punishing every
  reader to avoid fixing the compiler. Essentially only json follows it in
  46 std modules today, which is itself the verdict.
- **Rename the colliding std types one by one** (`iter.Step` → `IterStep`,
  …). Rejected: fixes exactly the collisions we know about, does nothing for
  two third-party `Config`s, and bakes the flat namespace deeper with every
  rename.
- **Unqualified-by-default with lazy collision errors** (keep today's implicit
  import, error only when an ambiguous name is used). Rejected: keeps types
  and functions asymmetric, keeps "where did this name come from" unanswerable
  without tooling, and produces late, far-from-cause errors.
- **`import X as Y` aliasing.** Not needed to fix anything here (qualified
  names are already collision-free); noted as possible future ergonomics for
  long rune names. Deliberately out of scope.
- **Do nothing.** The registry ships, the first pair of runes collides, and
  the language has no answer. Not viable.

## Drawbacks

- **The largest breaking change to date.** Every cross-module type annotation
  and construction site in user code needs a qualifier or a from-import.
  Mitigations: the error at every site is precise and names the fix; the
  executed-docs suite finds all in-repo sites mechanically; and pre-1.0 with
  a near-empty package ecosystem is the last cheap moment to do it.
- **Two resolution moments** (linker for expressions/annotations, typeck for
  scrutinee-scoped variant patterns) is real complexity, and the split must be
  documented in spec/architecture.md. The alternative — one moment — forfeits
  either bare match arms (ergonomics) or early errors.
- `fmt` cannot fully automate the migration (rewriting `JsonInt(1)` to
  `json.JsonInt(1)` needs resolution, not layout); the linker has the mapping,
  so a one-shot `witchy fix`-style assist is possible but is not promised here.
- Error messages must render qualified names carefully or the old
  `expected Secret, found Secret` confusion returns in a new costume — the
  diagnostic work is part of the definition of done, not optional polish.

## Prior art

- **Go packages** — the closest model and the proof it scales: types are
  reachable *only* qualified (`json.Decoder`, `http.Client`), no unqualified
  type imports at all, one flat name per package. Witchy adds `from`-imports on
  top because match-arm ergonomics matter more in an ADT language than in Go.
- **Python** — `import x` / `from x import y` surface copied outright;
  `import *` deliberately not copied.
- **Rust** — `use` paths and variant-scoping-with-type (`Enum::Variant`,
  with bare variants via `use Enum::*` or match ergonomics); witchy's §4 rule
  is Rust's match-position leniency without the `::` ceremony.
- [RFC-0002](0002-user-definable-capabilities.md) (link-time, home-module-aware
  sealing — the in-repo precedent for module-of-origin tracking) and
  [RFC-0018](0018-compiler-architecture.md) (the linker as the single
  whole-program merge point this RFC edits).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields, and appending
    dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
