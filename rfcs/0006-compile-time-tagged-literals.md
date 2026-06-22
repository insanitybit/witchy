---
rfc: 0006
title: Compile-time tagged literals
status: proposed        # proposed | planned | implemented | rejected | superseded
created: 2026-06-22
superseded-by:
tracking:
---

# RFC-0006: Compile-time tagged literals

> Provisional syntax. Code blocks here are intentionally **not** tagged `witchy`
> so the doc-examples test does not try to compile them.

## Summary

Add one general mechanism: a *tagged literal* `tag"…${expr}…"` that desugars **at
compile time** into a call `tag(static_parts, hole_expressions)`. The `tag` is an
ordinary `comptime` function; it runs during compilation and returns **AST**,
which is spliced into the program at the literal's site. This is the
TypeScript tagged-template idea (`` sql`…` ``, `` html`…` ``) but moved to compile
time: typed, validated before codegen, and producing AST/data rather than a
runtime string. One feature yields `html`, `sql`, `css`, and `regex` as ordinary
library functions — no bespoke JSX syntax, no per-DSL special-casing.

This is the foundational language feature underneath the browser-frontend set:
[`RFC-0007`](./0007-witchy-wasm-browser-target.md) *"witchy-WASM in the browser: a
pure-compute target"* provides the execution target, and
[`RFC-0008`](./0008-frontend-framework-rune.md) *"A capability-pure
frontend framework (MVU over VNode)"* is the primary consumer — its `html` tag is
built directly on this mechanism.

## Motivation

### Ergonomics without a special-cased syntax

Markup, queries, and regular expressions read best written *as themselves* —
`<div class="…">…</div>`, `SELECT … WHERE …`, `\d+`. Today the only way to author
those in witchy is a builder DSL (`element("div", [...], [...])`), which is verbose
and structurally noisy at exactly the nesting depth where readability matters most.
The TypeScript/React answer is JSX — but JSX is a bespoke grammar bolted onto the
language. A tagged literal gives the same authoring experience with **no new
syntax per DSL**: the surface is one prefix, `tag"…"`, and every embedded grammar
is just a library.

### Safety: the literal is checked at compile time

Because expansion happens during `comptime`, the literal is parsed, validated, and
type-checked **before** the program runs:

- Interpolation holes are typed **by position** (a text-position hole and an
  attribute-position hole admit different types — see *Typing*).
- There is **no runtime string parser** — itself an attack surface — and therefore
  no string-injection class of bug. An `html` tag produces `VNode` *data*, so a
  `${userInput}` in text position becomes a text node, never markup (see *The
  headline use*).

### "Generic over special"

The project value is to build the general mechanism, never special-case to dodge
implementation cost. Tagged literals are that mechanism: **one** typed,
compile-time splice facility, from which `html`/`sql`/`css`/`regex` fall out as
libraries. What goes wrong without it is one of two off-grain outcomes:

- a **bespoke JSX** — a special-cased, parallel grammar the rest of the toolchain
  must learn (the path [`RFC-0008`](./0008-frontend-framework-rune.md)
  explicitly declines), or
- **builder DSLs only** — no new feature, but the Elm-`Html`-style ergonomics gap:
  correct, capability-clean, and tediously verbose for real markup.

## Design

### Reuse the existing interpolation lexing

witchy's string lexer **already** splits an interpolated literal `"a${x}b${y}c"`
into a list of static parts (`["a", "b", "c"]`) and a list of hole expressions
(`[x, y]`). That is *exactly* the `(strings, ...values)` shape a tagged template
needs. The only new surface is the `tag` prefix that binds the literal to a
comptime function:

```text
let node = html"<p class=${cls}>Hello, ${name}!</p>"
```

lexes to the same static-parts/holes split as the bare string would, and desugars
(at compile time, before type-checking the surrounding code) to:

```text
html(["<p class=", ">Hello, ", "!</p>"], [cls, name])
```

No new lexer state machine: the prefix attaches an identifier to an interpolation
the lexer can already produce.

### The tag is a `comptime` function returning AST

witchy already has `comptime` — compile-time evaluation of ordinary functions.
What is **new** here is that a tag returns **AST to splice**, i.e. compile-time
*code generation* rather than just a compile-time *value*. A tag's contract:

```text
comptime fn html(parts: [StaticStr], holes: [Ast]) -> Ast
```

- `parts` are the static fragments delivered as **compile-time strings** — the tag
  parses them with its own embedded grammar (HTML, SQL, a regex, …).
- `holes` are the interpolation expressions delivered **as AST nodes**, not as
  evaluated values. The tag decides where each hole may appear and what AST to
  emit around it.
- the return is **AST**, spliced into the program at the literal's site, then
  type-checked as if the author had written it by hand.

A tag is therefore a small compiler that runs during compilation: it consumes a
fixed-shape literal and emits ordinary witchy AST (constructor calls, list
literals, matches). Nothing it emits is privileged — it is code the author *could*
have written, generated for them.

### Typing: holes are typed by position

The tag's grammar defines what each hole position admits; the spliced result is
type-checked normally afterward. For `html`:

- a hole in **text position** must be `String | VNode` (it becomes a child node),
- a hole in **attribute position** must be `Attr | String` (it becomes an
  attribute),
- a hole in a position the grammar does not allow (e.g. a tag *name* hole, if the
  grammar forbids it) is rejected by the tag itself.

A wrong-typed hole is a **compile error**, and the error must point **inside the
literal** — at the offending `${…}`, not at the desugared call. That requires
**column-mapping back into the source string**: the tag (and the splice machinery)
must carry source spans from each static fragment and each hole through expansion,
so a type error on the spliced node resolves to the original column within the
literal. This is a **real implementation cost**, called out here so it is budgeted:
without it, the diagnostics land on synthetic generated code and the feature's
safety story degrades to "it failed somewhere."

### Hygiene

Spliced AST resolves names in the **tag's defining scope**, not the call site.
When `html` emits constructor calls like `element(…)` / `text(…)`, those names
resolve to the constructors visible **where `html` is defined** (its module), not
to whatever happens to be named `element` at the call site. This prevents
accidental capture — a local `element` variable in user code cannot shadow or
hijack what the tag emits — and means the emitted constructors are a stable,
private contract of the tag's library. Holes, conversely, are the *author's*
expressions and resolve at the **call site** (that is their whole purpose). The
splice machinery keeps the two name-resolution origins separate: tag-emitted nodes
carry the tag's scope, hole nodes carry the call site's.

### Parity (the prime directive)

Expansion happens at **comptime, before codegen**. By the time either backend sees
the program, the tagged literal is *already gone* — replaced by ordinary AST. So
the interpreter and the compiled-WASM backend compile the **same expanded AST**
and there is **no new divergence surface**: a tag is not a runtime construct on
either backend, it is a compile-time rewrite that both backends consume identically.
A differential test compiles a tagged literal and asserts both backends agree on
the expanded program's behavior; the tag itself runs once, in the compiler.

### The headline use (forward-ref RFC-0008)

The motivating consumer is an `html` tag for
[`RFC-0008`](./0008-frontend-framework-rune.md)'s MVU-over-`VNode`
framework. Because `html` produces **`VNode` data** (not a string), interpolation
is structural:

```text
fn view(model: Model) -> VNode:
    html"<div class=${model.css}>${model.userInput}</div>"
```

`${model.userInput}` sits in text position, so it expands to a **text node**. There
is no code path by which a string of user input becomes markup — the tag never
emits a "parse this string as HTML" node — so the `html` tag is **XSS-immune by
construction**, not by escaping discipline. The same mechanism benefits:

- `sql"… WHERE id = ${id}"` — holes become **bound parameters**, never spliced
  into the query text, so the injection class is gone the same way.
- `regex"\d+${suffix}"` — compiled and validated at compile time; an invalid
  pattern is a compile error, not a runtime panic.
- `css"…${color}…"` — typed, scoped class generation with the same hole discipline.

Each is a library function. The language learns the mechanism once.

## Alternatives

- **A bespoke JSX syntax.** A parallel grammar special-cased into the
  lexer/parser, as TS/React do. Rejected per "generic over special": it solves
  *one* DSL (markup) with permanent language surface every tool must learn, and
  leaves `sql`/`css`/`regex` still wanting their own special cases. The tagged-
  literal mechanism subsumes JSX as a *library* and serves the others for free.
- **Builder DSLs only (do nothing).** Keep `element("div", attrs, kids)` and add
  no language feature. This is the honest baseline — zero new complexity — but it
  is the Elm-`Html` ergonomics gap: real markup nests deeply and the builder noise
  dominates. Rejected as the *only* option; builder constructors remain the
  emission target a tag *expands to*, so they are not wasted.
- **Runtime tagged templates (TypeScript-style).** Keep `tag(strings, values)` but
  run it at **runtime**, returning a runtime value. Rejected on three counts: it is
  **slower** (the embedded grammar is parsed on every call), **untyped** (the holes
  are runtime values, so position-typing and the compile-error story are lost), and
  for `html` it is an **XSS footgun** — a runtime `html` that concatenates strings
  reintroduces exactly the injection surface the compile-time form removes. Moving
  the work to comptime is the entire point.

## Drawbacks

- **This is a typed, hygienic macro facility.** Mechanically, that is what a
  compile-time function consuming a fixed-shape literal and emitting hygienic AST
  *is*. It is the **single largest language addition on the table** — comptime
  code generation, scope-aware splicing, and span propagation — and it should be
  weighed as such, not as "just string templates."
- **Tooling burden.** `fmt` must format *inside* the literals (it cannot treat
  `html"…"` as an opaque string and still reflow markup). The LSP must highlight
  and diagnose the **embedded** grammars — the "SQL strings get syntax
  highlighting" experience — which is a **separate, non-trivial tooling lift**, not
  a free consequence of the language feature. Shipping the mechanism does not ship
  the editor experience.
- **The error-span machinery is fiddly.** Column-mapping diagnostics back into the
  literal (see *Typing*) is the part most likely to be cut for v1 and most
  damaging to cut — bad spans turn a safety feature into a debugging chore.
- **Overuse / obfuscation risk.** A general AST-splicing facility invites clever
  DSLs that bury control flow in unreadable tags. The language gains expressive
  power that can be misused; convention and review, not the compiler, bound it.
- **One more compile-time phase.** Comptime expansion runs before type-checking the
  surrounding code, so a buggy tag fails compilation in a phase authors must learn
  to reason about. The payoff is that the failure is at compile time, not runtime.

## Prior art

- **TypeScript / JavaScript tagged template literals** — the `(strings, ...values)`
  shape and the `` sql`…` `` / `` html`…` `` idiom this generalizes; witchy moves
  it to compile time and types the holes.
- **JSX** — the ergonomics target for markup, subsumed here as a *library* tag
  rather than a bespoke grammar.
- **Rust procedural macros** — token-stream-in, token-stream-out compile-time code
  generation; the closest analog to "tag returns AST," including the span-tracking
  cost for good diagnostics.
- **Zig `comptime`** — compile-time evaluation as a first-class, ordinary-function
  facility; witchy's existing `comptime` is the base this extends to *emit* AST.
- **Lisp quasiquote and `syntax-rules` hygiene** — the hygienic-splice discipline
  (tag-emitted names resolve in the tag's scope, holes in the call site's) is the
  Scheme/Racket macro-hygiene model.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below (e.g. "> 2026-07-01: clarified X").
  - The current behavior lives in spec/ and the code — NOT here.
-->
