---
rfc: 0080
title: Structured hygienic metaprogramming
status: proposed
created: 2026-07-12
superseded-by:
tracking: "source-backed syntax wrappers/builders, comptime emit_item/fn helpers, typed custom derives and tags, parser-backed quotation/holes, witchy expand, and deterministic compiler-owned meta.fresh identifiers landed; definition/call-site origin hygiene and compiler-owned syntax trees remain proposed"
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

This is intentionally not the full RFC. The payload is still source-backed and
expression/type/pattern/statement/block/item quotation plus
expression/type/pattern holes plus mixed statement/block/item holes exist;
definition-site/call-site identifier origins and compiler-owned
expression/pattern/type syntax trees remain future work. The value is the
migration seam: future work can move the
payload behind these wrappers from parsed source to structured compiler nodes
without changing the comptime append/merge path again.
