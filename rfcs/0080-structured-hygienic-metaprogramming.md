---
rfc: 0080
title: Structured hygienic metaprogramming
status: proposed
created: 2026-07-12
superseded-by:
tracking: "source-backed meta.ItemSyntax, comptime emit_item, comptime fn helpers, and typed custom derives landed; quotation/hygiene/full syntax API remains proposed"
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
`TypeExpr`. The remaining generation boundary is still textual: `comptime`
uses `emit(String)`, and tags return expression source which the compiler
reparses.

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

This is intentionally not the full RFC. The payload is still source-backed and
there is no quotation, identifier hygiene, or structured expression/pattern/type
constructors yet. The value is the migration seam: future work can move the
payload behind `ItemSyntax` from parsed source to structured constructors
without changing the comptime append/merge path again.
