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
    print(console, "${lucky_0()} ${lucky_1()} ${lucky_2()}")
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
strings. When a generator actually needs to write a type into emitted source,
`meta.type_source(expr)` renders that structured value back to source text.

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

```witchy
import list

fn greet(parts: List(String), holes: List(String)) -> String:
    "\"Hello, \" + " + holes.at(0) + " + \"!\""

fn main(console: Console):
    let name = "witch"
    print(console, greet"hi ${name}")
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
