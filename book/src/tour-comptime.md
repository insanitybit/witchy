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
`quote expr:` values retain their parsed AST, including when they contain typed
holes. The compiler substitutes each hole node structurally instead of joining
and reparsing the enclosing expression. A source-backed compatibility hole may
still parse its own payload. Existing `meta.expr_*` builders project canonical
source, and values composed through those builders remain on the compatibility
path until their structural forms land.
`quote type:` values work the same way, including typed holes. The enclosing
type template remains compiler-owned, so nested anonymous structural types and
borrowed views are substituted as nodes. A source-backed compatibility hole may
parse its own payload; general `meta.type_*` builder composition still projects
canonical source.
`quote pattern:` values also retain their parsed node, including typed holes.
The compiler substitutes the exact pattern node, while a compatibility hole may
parse its own payload. General `meta.pattern_*` composition remains the
source-backed construction API.
`quote stmt:` and `quote block:` values retain their body AST too, including
mixed expression, type, and pattern holes. The existing body builders consume
their canonical projection, so generators can migrate without changing the
`meta.function_block` surface while the quoted body itself stays structural.
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

Statement and block templates accept mixed syntax categories. The block below
substitutes a compatibility-built type, pattern, and expression plus an owned
expression quote before the existing function builder consumes it:

```witchy
import meta

comptime fn generated_body() -> meta.BlockSyntax:
    let int = meta.type_named(meta.ident("Int"), [])
    let binding = meta.pattern_var(meta.ident("value"))
    let seed = meta.expr_int(40)
    let tail = quote expr:
        value + 2
    quote block:
        let x: ${int} = ${seed}
        let ${binding} = x
        ${tail}

comptime:
    let int = quote type:
        Int
    emit_item(meta.function_block(true, meta.ident("body_generated"), [], Some(int), generated_body()))

fn main(console: Console):
    console.print("${body_generated()}")
```

```text
42
```

Pattern templates follow the same rule. The compatibility-built `1` pattern is
inserted into an owned alternation, and that pattern is then inserted into the
generated match arm:

```witchy
import meta

comptime:
    let one = meta.pattern_int(1)
    let selected = quote pattern:
        ${one} | 2
    emit_item(quote item:
        pub fn selected_value(value: Int) -> Int:
            match value:
                ${selected} -> 42
                _ -> 0
    )

fn main(console: Console):
    console.print("${selected_value(2)}")
```

```text
42
```

Type templates also substitute structurally. Here the inner `Int` comes from a
compatibility builder, the anonymous record and `List` wrapper remain owned
types, and the generated function receives the final type without reparsing the
enclosing templates:

```witchy
import meta

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let row = quote type:
        .{value: ${int}}
    let rows = quote type:
        List(${row})
    emit_item(quote item:
        pub fn sum_first(values: ${rows}) -> Int:
            values.at(0).value
    )

fn main(console: Console):
    console.print("${sum_first([.{value: 42}])}")
```

```text
42
```

Expression templates can contain syntax that is not independently parseable as
a builder payload. The nested quote below preserves the anonymous-record AST
through both substitutions:

```witchy
import meta

comptime:
    let record = quote expr:
        .{value: 40}
    let body = quote expr:
        ${record}.value + 2
    emit_item(quote item:
        pub fn generated() -> Int:
            ${body}
    )

fn main(console: Console):
    console.print("${generated()}")
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

A typed generator may instead return `meta.ExprSyntax`. `quote expr:` results,
including structurally substituted expression templates, stay as compiler-owned
AST through this boundary; compatibility builders still project source until
their structural forms land. Direct functions, types, constructors, and
constructor patterns written in compiler-owned typed output resolve where the
tag is defined, including through that module's imports, while the literal's
holes keep their invocation-site context. This name resolution does not bypass
sealed-type construction rules. Use
`meta.call_site("name")` when generated code deliberately needs an invocation
scope identity. Pass it to `meta.expr_name`, `meta.type_named`, or
`meta.pattern_ctor` to choose an expression, type, or constructor-pattern
reference. These references remain compiler-owned nodes through structural
quotation; they are not encoded as generated source. Imported tags remain
available during expansion and are removed before runtime type checking. If a
direct tag expansion fails, its diagnostic names the literal's invocation line
and the tag function's defining module and line, so an imported generator does
not collapse to an unlocated generated-source error.

```witchy
fn answer_value() -> Int:
    40

comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        answer_value() + 2

comptime fn caller_answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_name(meta.call_site("answer_value"))

fn main(console: Console):
    let answer_value = fn() -> Int:
        0
    console.print("${answer"ignored"}")
    let caller_answer_value = caller_answer"ignored"
    console.print("${caller_answer_value()}")
```

```text
42
0
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
