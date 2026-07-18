---
rfc: 0080
title: Structured hygienic metaprogramming
status: proposed
created: 2026-07-12
superseded-by:
tracking: "source-backed compatibility builders, comptime emit_item/fn helpers, typed custom derives and tags, parser-backed quotation/holes, witchy expand, deterministic compiler-owned meta.fresh identifiers, compiler-owned item/expression/type/pattern/statement/block quotations with structural typed holes, direct AST transport for typed tags, block builders, and function bodies, definition-site function/type/constructor/pattern resolution, explicit meta.call_site value/function/type/constructor references, and nested tagged-expansion diagnostics carrying invocation, definition, generated-parent, and hole ancestry landed; general qualified/remaining compatibility-builder/item origins and persistent per-node spans remain proposed"
---

# RFC-0080: Structured hygienic metaprogramming

## Summary

Extend witchy's implemented `comptime`, custom-derive, and tagged-literal
facilities with a structured syntax API. Compile-time functions may inspect
typed declarations and construct expressions, patterns, types, functions, and
impls without rendering source strings. Expansion is hygienic, deterministic,
capability-free, visible to tooling, and is type-checked and footprint-analyzed
exactly like handwritten code.

This is the general mechanism for Ruby-shaped library DSLs without global
runtime mutation of the program.

## Motivation

RFC-0006 proved that compile-time DSLs are useful: `html"..."` can parse an
embedded language, preserve call-site holes, and emit safe ordinary witchy.
RFC-0069 then made declaration reflection structured through `TypeInfo` and
`TypeExpr`. The remaining generation boundary started textual: `comptime`
used `emit(String)`, and tags returned expression source which the compiler
reparsed. RFC-0080 is replacing those public boundaries with sealed syntax
values while the payload is still source-backed internally.

Text generation is adequate for a tagged literal but poor as witchy's general
metaprogramming identity:

- generators must quote and indent syntax correctly;
- names and source spans are reconstructed after rendering;
- declaration transforms cannot naturally preserve identity;
- formatters and language servers see generated text too late;
- every sophisticated generator grows a private source builder.

Witchy should support declarative APIs for routes, schemas, serialization,
command-line parsers, tests, database mappings, and protocol adapters as
libraries. Those libraries need more than string templates, but they do not
need ambient I/O or unrestricted compiler mutation.

## Design

### Structured syntax values

`std/meta` gains opaque compiler-owned syntax values:

```witchy
type ExprSyntax
type PatternSyntax
type TypeSyntax
type ItemSyntax
type ModuleSyntax
type Ident
type Span
```

They are not runtime-reflective values and cannot escape compile time. Public
constructors are ordinary typed functions such as:

```witchy
meta.call(callee: ExprSyntax, args: List(ExprSyntax)) -> ExprSyntax
meta.name(id: Ident) -> ExprSyntax
meta.function(name: Ident, params: List(ParamInfo), body: ExprSyntax) -> ItemSyntax
meta.impl_block(trait: TypeSyntax, target: TypeSyntax,
                items: List(ItemSyntax)) -> ItemSyntax
```

The initial API covers every stable grammar production needed by built-in
derives. Raw token construction is deliberately absent. A missing constructor
is an API gap to add, not a reason to concatenate source.

### Quotation and holes

Quotation provides the ergonomic construction surface:

```witchy
comptime fn derive_validate(info: TypeInfo) -> List(ItemSyntax):
    let checks = info.fields.map(fn(field):
        quote expr { self.${field.ident}.validate()? }
    )
    [quote item {
        impl Validate for ${info.type_syntax}:
            fn validate(let self) -> Result(Nil, ValidationError):
                ${meta.statements(checks)}
                Ok(Nil)
    }]
```

Quoted literal syntax is parsed by the normal parser. `${...}` inserts only a
syntax value of the category expected at that position. A category mismatch is
a compile error at the hole.

### Hygiene and identity

Identifiers have an origin:

- names written by the macro resolve in the defining module;
- syntax passed through a hole retains its original resolution context;
- `meta.fresh("tmp")` creates an unspellable fresh binding;
- `meta.call_site("name")` is the explicit escape hatch for resolving a name at
  the invocation site.

There is no implicit textual capture. Generated private names cannot collide
with user names.

### Expansion forms

Three forms share one expansion engine:

1. `comptime:` appends `ItemSyntax` values to its module.
2. Custom derives receive `TypeInfo` and return `List(ItemSyntax)`.
3. Tagged literals return `ExprSyntax`, retaining RFC-0006's opaque call-site
   hole substitution and embedded-language diagnostics.

Existing source-emitting forms remain during migration. Mixing source and
structured output in one expansion is rejected so ordering is unambiguous.

Macros may add declarations but may not rewrite or delete declarations they did
not create. Attribute-style input is permitted, but an attribute expands beside
its target rather than mutating the target's meaning invisibly.

### Determinism and authority

Expansion runs in the existing zero-capability compile-time evaluator. Syntax
values cannot contain a live capability. Generated source enters normal linking,
typechecking, capability-footprint analysis, and build-footprint comparison
after expansion. A macro can generate a function requiring `Net`; it cannot
grant `Net`, hide that requirement, or perform network I/O while expanding.

Expansion has explicit recursion, item-count, and evaluator-step limits. The
same source and compiler version must produce the same expanded module.

### Tooling contract

Every generated node carries its macro definition span, invocation span, and
hole ancestry. Diagnostics default to the invocation and can show an expansion
trace. `witchy expand` prints stable formatted expanded source. The LSP indexes
generated symbols and marks them as generated; go-to-definition can move from a
generated declaration to its macro invocation and definition.

## Alternatives

- **Keep source emission only.** Simple, already implemented, and sufficient for
  small derives. Rejected as the long-term model because it makes hygiene,
  identity, spans, and tooling conventions rather than structured guarantees.
- **Procedural compiler plugins.** More powerful, but they add ambient native
  code to the compiler process and bypass witchy's capability and reproducibility
  story.
- **Runtime Ruby-style class mutation.** Useful for some interception patterns,
  but it does not solve compile-time DSL validation and makes program meaning
  depend on load order. RFC-0084 proposes a scoped runtime mechanism separately.

## Drawbacks

- The syntax API becomes a compiler compatibility surface.
- Quotation adds parser and formatter work.
- Expansion-aware LSP behavior is a substantial tooling requirement.
- Powerful macros can obscure control flow even when hygienic. Libraries should
  prefer ordinary functions unless generation removes real repetition or adds
  compile-time validation.

## Prior art

Rust procedural and declarative macros, Scala 3 quotes, Template Haskell, Zig
comptime, Elixir quoted ASTs, and Racket syntax objects inform this design.

## Implementation note (2026-07-13) — first slice

The first source-compatible slice is implemented:

- `std/meta` exposes sealed `ItemSyntax` and `meta.item(source)`.
- Every `comptime:` block receives a compiler-injected `emit_item(item:
  meta.ItemSyntax)` helper alongside legacy `emit(String)`.
- `derive(...)` desugaring initially appended through
  `emit_item(item(generator(typeInfo)))`, with `item` from the compiler-injected
  `meta` imports, so the compiler-generated append
  boundary is typed even while built-in and custom derive generators continue to
  return source strings.
- The second slice moves built-in `std/meta.derive_*` generators to return
  `ItemSyntax` directly.
- The third slice separates the legacy source output channel (`emit` and direct
  `console.print` compatibility output) from the typed `emit_item(ItemSyntax)`
  channel and rejects a single `comptime:` block that mixes them.
- The fourth slice makes `meta.ItemSyntax` compile-time-only in the checker:
  runtime modules cannot mention it in signatures/fields/aliases or construct it
  through expressions such as `meta.item(...)`. The synthetic `comptime` module
  and `std/meta` remain the allowed homes.
- The fifth slice adds top-level `comptime fn` helpers. They may use
  compile-time-only syntax values, are callable during compile-time expansion,
  and are stripped before the runtime module is linked and type-checked.
- The sixth slice lets local user-defined custom derives return `ItemSyntax` or
  `List(ItemSyntax)` directly. Legacy source-string custom derives remain
  supported as the compatibility path.
- The seventh slice adds source-backed `TypeSyntax`, `ExprSyntax`, and
  `ParamSyntax` wrappers plus small constructors such as `type_named`,
  `expr_call`, `param`, and `function`. These are compile-time-only like
  `ItemSyntax`: they reduce whole-item string templates without pretending to
  provide quotation, identifier validation, or hygiene yet.
- The eighth slice adds compile-time-only `Ident` plus `meta.ident`, moves
  identifier positions in the source-backed builders to validated identifiers,
  and rejects keywords, `_`, non-ASCII spelling, and compiler-reserved `__`
  names before generated source is parsed. This is still validation, not
  hygienic origin tracking.
- The ninth slice adds source-backed `PatternSyntax`, `StmtSyntax`,
  `BlockSyntax`, and `MatchArmSyntax` wrappers plus builders for anonymous-union
  patterns, match expressions, let/return/expression statements, blocks, and
  block-bodied functions. This lets generators compose ordinary control-flow
  bodies without one whole-function string template, while preserving the same
  compile-time-only boundary as `ItemSyntax`.
- The tenth slice lets tagged-literal generators return `meta.ExprSyntax`
  directly. Legacy `String` tags remain supported, but a `comptime fn` tag can
  now return the sealed typed expression wrapper; the compiler-generated harness
  unwraps it internally, then applies RFC-0006 opaque hole substitution exactly
  as before.
- The eleventh slice adds parser-backed `quote expr:`. The parser parses
  the quoted expression at the quote site, renders it to canonical source, lowers
  the form to the sealed source-backed `meta.expr_raw(...)` constructor, and
  auto-imports `meta` for the generated call. This removes unchecked expression
  string spelling from the common typed-tag case without claiming item quotation,
  holes, or hygiene.
- The twelfth slice adds `quote type:` for named/generic, module-qualified,
  tuple, function, ownership-qualified, and capability-right types. It lowers to
  structured `TypeSyntax` builders (`type_named`, `type_tuple`, `type_fn`,
  `type_unique`, and friends), not to a new raw type-string constructor.
  Anonymous structural type quotation remains a follow-up because preserving
  those shapes needs real compiler-owned type syntax nodes.
- The thirteenth slice adds `quote pattern:` for the current pattern AST:
  variables, wildcards, literals, constructor and qualified-constructor
  patterns, anonymous-union patterns, tuples, list rests, integer ranges,
  durations, and or-patterns. It lowers to structured `PatternSyntax` builders,
  including a small string-literal renderer inside `std/meta`, rather than a raw
  pattern-string constructor.
- The fourteenth slice adds `quote item:` for one module item. It parses the item
  at the quote site, renders canonical source through the formatter, and lowers
  to the existing `meta.item(...)` typed item boundary. This gives users a
  checked item quotation form without adding a second item-construction API.
- The fifteenth slice adds expression holes inside `quote expr:`. A `${...}`
  hole is accepted only inside the quoted expression body, its contents are
  ordinary compile-time code that must evaluate to `meta.ExprSyntax`, and the
  parser lowers the quote to `meta.expr_join(parts, holes)`. This composes with
  typed tagged literals: a tag can wrap an opaque RFC-0006 hole marker as
  `ExprSyntax` and splice it into parser-checked quoted code.
- The sixteenth slice adds the same hole model to `quote type:` and
  `quote pattern:`. A type hole must evaluate to `meta.TypeSyntax`; a pattern
  hole must evaluate to `meta.PatternSyntax`. Quotes without holes keep the
  structured builder lowering, while quotes with holes lower to typed
  `meta.type_join(parts, holes)` / `meta.pattern_join(parts, holes)` boundaries.
- The seventeenth slice adds parser-backed `quote stmt:` and `quote block:`.
  They parse one statement or one whole block at the quote site, render canonical
  source, and lower through typed `meta.stmt_raw(...)` /
  `meta.block_raw(...)` wrappers. This completes the source-backed quotation
  categories needed by `meta.function_block` without claiming statement/block
  holes yet.
- The eighteenth slice adds expression holes inside `quote stmt:` and
  `quote block:`. A `${...}` in an expression position must evaluate to
  `meta.ExprSyntax`, and the parser lowers the quote through typed
  `meta.stmt_join(parts, holes)` / `meta.block_join(parts, holes)` boundaries.
- The nineteenth slice extends statement/block holes to type and pattern
  positions with a typed `meta.SyntaxHole` union. Mixed statement/block quotes
  lower to `meta.stmt_join_syntax(parts, holes)` /
  `meta.block_join_syntax(parts, holes)`, where every splice is wrapped as
  `meta.expr_hole`, `meta.type_hole`, or `meta.pattern_hole` according to the
  grammar position it occupied.
- The twentieth slice extends the same mixed-hole model to `quote item:`.
  Expression holes are collected from function/default/comptime bodies and
  constants, while type and pattern holes are preserved from the quoted item
  grammar. Mixed item quotes lower to `meta.item_join_syntax(parts, holes)` so
  whole generated declarations can stay on the typed syntax boundary instead of
  reopening a raw source template at the item edge.
- The twenty-first slice adds `witchy expand <file.witchy>`. It loads the entry
  file plus sibling import sources for tag resolution, runs the same
  `comptime:` and tagged-literal expansion pass used before linking, and prints
  only the expanded entry module as canonical source. It is deliberately not a
  full linked dump and does not type-check or compile the program.
- The twenty-second slice adds `meta.fresh(hint) -> Ident`. The compiler gives
  each comptime block and tagged-literal invocation a deterministic namespace,
  then allocates monotonically within it. The rendered identifier uses the
  source-reserved `__` namespace, so repeated calls are distinct, identical
  source rebuilds are stable, and handwritten bindings cannot capture a fresh
  generated binding. This is the first concrete hygiene guarantee; ordinary
  quoted identifiers are not yet definition-site/call-site syntax objects.
- The twenty-third slice moves hole-free `quote item:` and literal whole-item
  `meta.item("...")` values off the source payload. The parser stores the
  original `Item` in a module-owned syntax table and emits an unspellable,
  deterministic handle. The compile-time evaluator forwards that opaque value,
  and `emit_item` returns the original node to the expander in an ordered typed
  event stream, so append no longer formats or reparses it. The table is
  target-neutral in-memory AST, including in the browser compiler; it does not
  depend on the native-only persistent-cache serializer. Dynamic item strings,
  item builders, and item quotations containing holes retain the compatibility
  source path. `witchy fmt` prints the canonical `meta.item("...")` spelling,
  which the parser promotes back to the same compiler-owned node.
- The twenty-fourth slice keeps hole-bearing `quote item:` on that same
  compiler-owned item channel. The parser stores one item AST containing exact
  expression, type, and pattern placeholder nodes. The compile-time evaluator
  unwraps each typed `SyntaxHole`, parses only its still-compatible payload, and
  substitutes that node into a clone of the item template; it never assembles or
  reparses the enclosing declaration. Literal `meta.item_join_syntax(parts,
  holes)` plans are promoted to the same representation, and formatting prints
  that public typed form and reconstructs the compiler table on parse. Hole
  payload syntax remains source-backed until the expression/type/pattern slices.
- The twenty-fifth slice gives hole-free `quote expr:` and literal
  `meta.expr_raw("...")` a module-owned expression AST and deterministic private
  handle. The sealed syntax value carries an internal AST identity plus canonical
  source projection. A direct expression hole retrieves the AST, including
  anonymous-record expressions that the compatibility payload parser cannot
  represent alone. Existing `meta.expr_*` builders project source and return a
  compatibility value, preserving their API while structural builders remain
  future work. Formatting prints
  `meta.expr_raw("...")` and promotes literal payloads back where independently
  parseable. At this slice, expression quotes containing holes remain
  source-backed.
- The twenty-sixth slice gives typed tagged literals a compiler-owned expression
  event on the same interpreter expansion channel as typed item emission. A tag
  returning a hole-free quoted expression transfers the stored AST directly,
  after which RFC-0006 substitutes call-site hole nodes as before. A composed
  source-backed `ExprSyntax` is carried as an explicit compatibility event and
  parsed with the tag's qualifier context; a legacy `String` tag is unchanged.
  Missing, duplicate, cross-category, or mixed source/typed emissions fail
  loudly. Both runtime backends continue to consume only the expanded AST.
- The twenty-seventh slice gives hole-free `quote type:` a module-owned `Type`
  AST and deterministic private handle. Direct type holes retrieve that node
  without reparsing, including anonymous record/union types and RFC-0083
  borrowed views that the former builder lowering rejected. Existing
  `meta.type_*` builders project canonical source and remain compatible.
  Formatting uses a literal zero-hole `meta.type_join` plan and the parser
  promotes that spelling back to the same owned representation, so no public
  raw type-string constructor is added.
- The twenty-eighth slice gives hole-free `quote pattern:` a module-owned
  `Pattern` AST and deterministic private handle. Direct pattern holes retrieve
  that node without reparsing. Existing `meta.pattern_*` builders continue to
  project canonical source, while formatting uses a literal zero-hole
  `meta.pattern_join` plan that the parser promotes back to owned syntax.
- The twenty-ninth slice gives hole-free `quote stmt:` and `quote block:`
  module-owned `Stmt` and `Block` AST plus deterministic private handles.
  Existing body builders project canonical source, preserving
  `meta.function_block` compatibility. Formatting emits literal
  `meta.stmt_raw` / `meta.block_raw` projections that the parser promotes back
  to the owned nodes when they contain one valid statement or block.
- The thirtieth slice records the definition module when a typed tag emits a
  compiler-owned expression. Direct calls and direct function references written
  in that expression are resolved in the tag's defining module before call-site
  holes are substituted. The expander carries that decision through an
  unspellable linker marker, so private helpers remain reachable and a same-named
  local at the invocation cannot capture the generated reference. Generated
  lexical bindings inside the expression still shadow normally. Source-backed
  compatibility output, constructors/types, composed syntax, and the explicit
  `meta.call_site` escape remain later origin-model slices. The linker preserves
  imported compile-time tag functions plus their syntax tables in sibling
  expansion snapshots, while each module's runtime result remains stripped.
- The thirty-first slice moves hole-bearing `quote expr:` onto a compiler-owned
  expression-template channel. The parser stores one `Expr` AST containing
  exact expression-hole nodes, and the compile-time evaluator substitutes each
  typed `ExprSyntax` into a clone of that template. A compiler-owned hole is
  transferred as an AST; a compatibility hole parses only its own payload. The
  enclosing expression is never assembled or reparsed. Literal
  `meta.expr_join(parts, holes)` plans are promoted to the same representation
  when parsed, so formatting retains the public typed spelling while restoring
  the owned template. General `meta.expr_*` builder composition and
  hole-bearing type, pattern, statement, and block quotations remain on their
  compatibility paths.
- The thirty-second slice moves hole-bearing `quote type:` onto the same
  compiler-owned template model. The parser stores one `Type` AST containing
  exact type-hole nodes, and compile-time evaluation substitutes each typed
  `TypeSyntax` into a clone. Compiler-owned holes transfer their AST directly;
  compatibility holes parse only their own payload. Literal
  `meta.type_join(parts, holes)` plans are promoted to the same representation
  when parsed, while formatting preserves that public typed spelling. General
  `meta.type_*` builder composition and hole-bearing pattern, statement, and
  block quotations remain source-backed.
- The thirty-third slice moves hole-bearing `quote pattern:` onto a
  compiler-owned pattern-template channel. The parser stores one `Pattern` AST
  containing exact pattern-hole nodes, and compile-time evaluation substitutes
  each typed `PatternSyntax` into a clone. Compiler-owned holes transfer their
  AST directly; compatibility holes parse only their payload. Literal
  `meta.pattern_join(parts, holes)` plans are promoted to the same
  representation, and formatting restores that public typed spelling. General
  `meta.pattern_*` builder composition and hole-bearing statement/block
  quotations remain source-backed.
- The thirty-fourth slice moves hole-bearing `quote stmt:` and `quote block:`
  onto compiler-owned body templates. Their existing mixed `SyntaxHole`
  envelope preserves source order across expression, type, and pattern holes,
  while compile-time evaluation decodes each typed value and substitutes the
  corresponding AST node into a cloned `Stmt` or `Block`. Literal
  `meta.stmt_join_syntax(parts, holes)` and
  `meta.block_join_syntax(parts, holes)` plans are promoted to the same owned
  representation on parse/format round-trip. Current body builders may still
  project canonical source when constructing an item; the quotation payload
  itself is no longer assembled or reparsed.
- The thirty-fifth slice adds the explicit invocation-site escape for expression
  references. `meta.call_site(name)` validates a lowercase value/function
  identifier and carries a
  distinct `Ident` origin; `meta.expr_name` converts that value directly into a
  compiler-owned `Expr` node rather than rendering a reserved source spelling.
  Definition-site marking deliberately preserves the opaque origin through typed
  tag expansion, and the consumer link resolves it against the invocation's
  lexical bindings and functions. Both direct function-value references and
  calls formed by structural quotation are covered, and neither the call-site nor
  definition-site marker reaches type checking. Other `Ident` consumers still use
  their compatibility spelling, so constructor, type, pattern, field, and item
  origins remain later slices rather than silently claiming general hygiene.
- The thirty-sixth slice makes direct tagged-expansion failures carry both ends
  of their provenance. Local and imported typed or legacy tags report the
  consumer module and tagged-literal invocation line plus the defining module
  and function line. Link, type-check, evaluator, generated-source parse, typed
  emission, and hole-substitution failures all pass through that one expansion
  trace. This does not yet attach a full span/ancestry object to every generated
  AST node; nested-node diagnostics and LSP generated-symbol navigation remain
  later tooling slices.
- The thirty-seventh slice threads expansion ancestry through recursively emitted
  tagged literals. A failing generated inner tag reports the outer invocation and
  definition frame; a failing tag parsed from a call-site hole additionally
  reports the hole index and explicitly labeled hole-local line/column. Hole
  markers are substituted before recursive expansion, preserving generated-tree
  order: dropped holes are not expanded, reordered holes follow placement order,
  and duplicated holes receive independent invocation identities. A temporary
  compiler-only wrapper carries each placement's hole ancestry through that walk
  and is removed before type checking. This ancestry is diagnostic state during
  expansion; it does not yet claim persistent spans on every generated AST node.
- The thirty-eighth slice extends definition-site identity across every written
  type, trait, constructor expression, and constructor pattern in a
  compiler-owned typed-tag expression. The defining scope resolves and validates
  those names before call-site holes are substituted, then seals qualified
  targets behind compiler-only markers. The consumer verifies and removes the
  markers without requiring the defining module's private or transitive imports
  to be imported again. A consumer declaration with the same spelling cannot
  capture generated syntax, while an opaque hole containing that spelling still
  resolves at the invocation site. The explicit `meta.call_site` escape remains
  value/function-only; compatibility builders and item identifiers still need
  their own structured origin channels. Definition-site identity does not alter
  sealed-type construction authority. The accompanying enforcement audit closes
  the previously untyped `value.field = replacement` path: record updates infer
  the sealed type's canonical owner (including ambient stdlib types) and remain
  legal only in that defining module.
- The thirty-ninth slice completes the explicit invocation-site escape for the
  compiler-owned expression surface. `meta.call_site(name)` accepts any valid
  identifier; the consuming constructor assigns its category:
  `meta.expr_name` retains value/function or constructor identity,
  `meta.type_named` retains type identity and structural type arguments, and
  `meta.pattern_ctor` retains constructor-pattern identity and structural
  subpatterns. Uppercase expression references become nullary constructors or,
  when applied, constructor calls in the consumer scope. All three origins stay
  as unspellable AST markers through definition-site expansion and are consumed
  before type checking. A call-site type alias expands only from the consumer's
  alias environment; a same-spelled alias in the generator module cannot capture
  it. Qualified-name composition and source-projecting item builders remain later
  origin work.
- The fortieth slice records provenance for every node in a generated item, not
  only its item root. The compiler assigns deterministic DFS structural paths to
  nested expressions, types, patterns, statements, and blocks, each retaining
  the generated item's definition span, invocation span, and hole ancestry.
  `OriginTable` supports exact path lookup, and remapping/appending continue to
  preserve those paths as item indices move. This gives diagnostics and tooling
  persistent per-node expansion provenance without adding rendered-source IDs or
  changing the runtime AST.
- The forty-first slice makes `meta.expr_call` structural when composing
  compiler-owned expression values. In particular, an explicit
  `meta.call_site` callee remains an invocation-site reference through the
  constructed `Apply` node instead of being projected to source and accidentally
  captured by the definition module. Source-backed `ExprSyntax` inputs remain a
  compatibility fallback and are parsed individually; the enclosing call is
  always represented by one compiler-owned AST node.
- The forty-second slice makes `meta.expr_field` structural for the same
  reason: an owned base expression, including `meta.call_site`, stays attached
  to the constructed `Field` node. Field names remain validated identifiers,
  because member selection is not a lexical binding position.
- The forty-third slice makes `meta.expr_match` preserve its compiler-owned
  scrutinee. `MatchArmSyntax` deliberately remains source-backed until its own
  node representation exists; each arm is parsed as an individual compatibility
  payload, never by projecting and reparsing the scrutinee or enclosing match.
- The forty-fourth slice makes `meta.function_block` produce a compiler-owned
  `Item::Function`. Its compatibility signature is parsed once, while an owned
  `BlockSyntax` body is transferred directly into the function item. Explicit
  call-site references and nested syntax identity therefore survive library
  generators instead of being erased by rendering the body to source.
- The forty-fifth slice makes `meta.block` produce a compiler-owned `Block`.
  Owned `StmtSyntax` elements and the optional owned tail `ExprSyntax` transfer
  directly into that block, preserving their internal origin markers across
  composition and subsequent `meta.function_block` construction.

This is intentionally not the full RFC. Every quotation category and its typed
hole placement is now compiler-owned. General `meta.*` builder composition may
still project canonical source, but `meta.block` and `meta.function_block`
retain owned child nodes when constructing blocks and items. Compiler-owned
typed tag expressions preserve
definition-site direct function, type, constructor, and constructor-pattern
references, and
`meta.call_site("name")`, consumed through `meta.expr_name`, `meta.type_named`,
or `meta.pattern_ctor`, explicitly selects invocation-site value, type, or
constructor resolution. Direct tagged-expansion failures preserve the invocation
and definition module/line pair. General qualified-name composition,
source-projecting compatibility-builder origins, and item/field identities
remain future work. The value is the
migration seam: future work can move the
payload behind these wrappers from parsed source to structured compiler nodes
without changing the comptime append/merge path again.
