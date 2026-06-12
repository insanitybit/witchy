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

## When to reach for it

Use `comptime` for families of declarations that follow a pattern — lookup
tables, wrapper functions, enumerated constants. For generating code from
*files* (a schema, a protocol definition), use a [build step](packages-build.md)
instead: build steps run in the build sandbox with explicitly granted read
roots, and their output becomes a separate generated module.

Next: the heart of witchy — capabilities.
