# Compile-Time Code: `comptime`

Sometimes the most honest way to write repetitive code is to *generate* it. A
top-level `comptime:` block is witchy's tool for that — a block that runs **at
compile time** and emits new source:

```witchy
comptime:
    var i = 0
    while i < 3:
        emit("pub fn lucky_${i}() -> Int:")
        emit("    ${i * 7}")
        emit("")
        i = i + 1

fn main(console: Console):
    console.print("${lucky_0()} ${lucky_1()} ${lucky_2()}")
```

```text
0 7 14
```

`emit(line)` is the block's only output channel. Everything it emits is
concatenated, parsed as witchy source, and **appended** to the module — and
only then does the module get type-checked and footprint-analyzed. Generated
code is held to exactly the same rules as handwritten code.

## Why it can't break the capability model

Code generation is where macro systems usually punch holes in language
guarantees. `comptime` is shaped so it can't:

- **No capabilities.** A `comptime:` block has no parameter list, so there is
  nothing to receive a `Console`, `Dir`, or `Net` through — and capabilities
  cannot be forged. No filesystem, no network, no clock: the block is
  **deterministic by construction**. The same source always generates the
  same code, which is also what makes builds cacheable.
- **Additive only.** Emitted source is appended. A `comptime` block cannot
  rewrite or delete existing items, so it can't change what a signature
  *means* — it can't launder authority out of a parameter list someone
  already reviewed.
- **Analyzed after expansion.** `witchy caps`, the type checker, and the
  sandbox footprint all run on the *expanded* module. If generated code
  demands authority, it shows up in the footprint like anything else.

Two consequences worth knowing: a `comptime` block may not emit another
`comptime` block, and if the emitted text fails to parse, the compiler shows
you exactly what was emitted alongside the error.

## Type structure at compile time

Every `comptime:` block can read `module_types`, a list of `meta.TypeInfo`
values for the types declared in the module. Field and variant payload types are
available as structured `meta.TypeExpr` data, so generators can branch on
`TNamed("List", ...)` or `TNamed("Option", ...)` without parsing rendered type
strings. Declaration shape is a `meta.TypeKind`: `TypeRecord`, `TypeSum`, or
`TypeUninhabited`. When a generator actually needs to write a type into emitted
source, `meta.type_source(expr)` renders that structured value back to source
text.

## Typed generation and fresh names

For structured generators, prefer `emit_item(meta.ItemSyntax)` to raw `emit`
lines. The `meta` builders validate syntax categories before the generated item
is appended. `meta.fresh(hint)` creates a deterministic compiler-owned identifier
for a generated binding; user source cannot spell its reserved name, so a local
with the same human-readable hint cannot capture it.

`quote item:` remains compiler-owned syntax from parsing through append; it is
not rendered and reparsed. Typed holes replace exact expression, type, or pattern
nodes in that item AST. The hole payload values and dynamic builders remain the
compatibility path while the rest of the syntax tree becomes structural.
Hole-free `quote expr:` values also retain their parsed AST when passed directly
through an item hole. Existing `meta.expr_*` builders can consume them by
projecting canonical source; the newly composed value remains a compatibility
value until structural expression builders land.
Hole-free `quote type:` values work the same way: a direct type hole receives
the parsed type node, including anonymous structural types and borrowed views,
while `meta.type_*` builders may still compose through canonical source.
Hole-free `quote pattern:` values also retain their parsed node through direct
item holes; `meta.pattern_*` remains the compatibility construction API.
Hole-free `quote stmt:` and `quote block:` values retain their body AST too.
The existing body builders consume their canonical projection, so generators
can migrate without changing the `meta.function_block` surface.
When a typed tagged-literal generator returns one of these compiler-owned values,
the expansion engine transfers the expression AST directly. A composed
source-backed `ExprSyntax` and a legacy `String` tag retain the explicit parse
fallback.

```witchy
import meta

comptime:
    let int = quote type:
        Int
    let value = meta.fresh("value")
    emit_item(meta.function(true, meta.ident("identity"), [meta.param(value, int)], Some(int), meta.expr_name(value)))

fn main(console: Console):
    console.print("${identity(42)}")
```

```text
42
```

## Tagged literals: `comptime` in expression position

A string literal written *immediately after an identifier* — `tag"…"` — is a
**tagged literal**. Like `comptime` it runs at compile time, but in *expression*
position: the lexer splits the literal into its static fragments and its `${…}`
holes, and the compiler calls your `tag` function

```text
fn tag(parts: List(String), holes: List(String)) -> String
```

with `parts` = the static fragments and `holes` = one **opaque marker** per hole.
The tag *places* each marker where that hole's value belongs and returns witchy
**expression source**; the compiler parses it and substitutes the real hole
expression — resolved at the call site — at each marker, then splices the result
in before type-checking.

A typed generator may instead return `meta.ExprSyntax`. Hole-free `quote expr:`
results stay as compiler-owned AST through this boundary; compatibility builders
still project source until their structural forms land. Direct function calls
written in compiler-owned typed output resolve where the tag is defined, while
the literal's holes keep their invocation-site context. Imported tags remain
available during expansion and are removed before runtime type checking.

```witchy
fn answer_value() -> Int:
    40

comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        answer_value() + 2

fn main(console: Console):
    let answer_value = fn() -> Int:
        0
    console.print("${answer"ignored"}")
```

```text
42
```

```witchy
import list

fn greet(parts: List(String), holes: List(String)) -> String:
    "\"Hello, \" + " + holes.at(0) + " + \"!\""

fn main(console: Console):
    let name = "witch"
    console.print(greet"hi ${name}")
```

```text
Hello, witch!
```

The tag runs **once, in the compiler**; both backends then compile the same AST.
Holes are typed by position (the substituted expression is type-checked normally),
so a type error points back *into the literal* at that `${…}`, and a marker may be
placed zero, once, or many times. The payoff is safety by construction: the
`glamour` rune's `html` tag turns a `${userInput}` in text position into a
`text(…)` **node**, never markup — so interpolated input is **XSS-immune**, not
escaped-after-the-fact.

## When to reach for it

Use `comptime` for families of declarations that follow a pattern — lookup
tables, wrapper functions, enumerated constants. Reach for a **tagged literal**
when you want a compile-time mini-language in expression position (typed
templates, safe HTML/SQL). For generating code from *files* (a schema, a protocol
definition), use a [build step](packages-build.md) instead: build steps run in the
build sandbox with explicitly granted read roots, and their output becomes a
separate generated module.

Next: the heart of witchy — capabilities.
