# Modules and Source Files

A `.witchy` file is a module. Its filename supplies the module name: code in
`parser.witchy` is imported as `parser`. Items are private to their module unless
declared `pub`.

```text
// parser.witchy
fn digit_value(s: String) -> Int:
    s.to_int()

pub fn parse_count(s: String) -> Result(Int, String):
    match s.parse_int():
        Some(n) -> Ok(n)
        None -> Err("not an integer: ${s}")
```

Another file imports the module and calls its public function by its qualified
name:

```text
import parser

fn load_count(raw: String) -> Result(Int, String):
    parser.parse_count(raw)
```

`parser.digit_value` is private and cannot be called from the importing file.
Qualification makes the owner of a name visible and prevents two imports from
silently choosing between same-named functions.

## Imports and constructors

`import name` brings a module into scope. Functions remain qualified, as in
`json.decode(text)`. Public types and constructors are qualified too:
`json.Json`, `json.JsonInt(1)`, and `json.JsonObject(fields)`.

`from name import Type` imports one type and its variant constructors. This is
useful when a type appears throughout a module:

```witchy
import json
from json import Json

fn describe(value: Json) -> String:
    match value:
        JsonInt(n) -> "integer ${n}"
        JsonString(s) -> "string ${s}"
        _ -> "another JSON value"

fn main(console: Console):
    let value = json.decode("7") ?? JsonInt(0)
    console.print(describe(value))
```

```text
integer 7
```

A constructor may also be written bare in a `match` when the scrutinee's type
already identifies it. Two unqualified imports that would define the same name
are rejected at the import line.

## The prelude

Eight bundled modules are always available: `list`, `string`, `dict`, `math`,
`option`, `result`, `policy`, and `show`. Writing `import list` is accepted but
redundant. Other standard-library modules, including `json`, `iter`, `time`,
`bytes`, and `testing`, require an import.

Capability operations follow a different naming rule. They are methods on the
capability value that authorizes them: `console.print(text)`, `dir.read(path)`,
and `clock.now()`. Importing a module never grants authority. `fail(message)` is
the one bare effect-free builtin.

## Module constants

A top-level `let` declares a module constant. It is evaluated and inlined during
linking, so it does not create mutable global state:

```text
let default_port = 443

pub fn endpoint(host: String) -> String:
    "${host}:${default_port}"
```

Top-level `var` is not a global-state escape hatch. Mutable state belongs in a
function, a value threaded through calls, a task, or an explicitly authorized
external resource.

## Resolution and packages

Bundled standard-library names are reserved and always resolve to the compiler's
shipped modules. Other imports resolve a sibling file or a rune dependency from
`witchy.toml`. The linker combines those modules, expands compile-time code, and
then type-checks one program; module privacy and qualified names still determine
which source-level references are legal.

The [runes and registry](packages.md) chapters cover dependency resolution. The
next chapter covers source that generates additional items before linking and
type checking finish.
