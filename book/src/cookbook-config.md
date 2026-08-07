# Parsing Configuration with TOML

witchy programs read configuration through the `toml` module. It offers two
levels of access: a quick path-based lookup for one-off values, and a typed
decode for validating a whole document. Both are pure string operations — no
capability required to parse text you already hold.

## Quick lookups with `get`

When you just need a value or two, `toml.get(text, path)` walks a dotted path and
returns an `Option(String)`. It returns `None` for a missing key, so you handle
the absent case explicitly.

```witchy
import toml

fn main(console: Console):
    let config = "title = \"my app\"\nport = 8080\n\n[owner]\nname = \"ada\""
    match toml.get(config, "title"):
        Some(v) -> console.print("title: ${v}")
        None -> console.print("no title")
    match toml.get(config, "owner.name"):
        Some(v) -> console.print("owner: ${v}")
        None -> console.print("no owner")
    match toml.get(config, "port"):
        Some(v) -> console.print("port: ${v}")
        None -> console.print("no port")
```

```text
title: my app
owner: ada
port: 8080
```

The dotted path (`owner.name`) reaches into a `[owner]` table. `get_array` and
`table` handle arrays and whole sections when you need more than a scalar.

## Typed decoding for whole documents

For real configuration loading — where a wrong type or missing field should be a
clean error, not a silent default — decode the document once and pull typed
fields out of the resulting `Toml` tree. Every step returns a `Result`, so a
malformed file surfaces as an `Err` you can report, and the `?` operator chains
the whole thing.

```witchy
import toml

fn main(console: Console):
    let src = "name = \"witchy\"\nversion = \"0.1.0\""
    match toml.decode(src):
        Ok(doc) ->
            match toml.table_field(doc, "name"):
                Some(node) ->
                    match toml.as_string(node, "name"):
                        Ok(s) -> console.print("name = ${s}")
                        Err(e) -> console.print("type error: ${e}")
                None -> console.print("no name field")
        Err(e) -> console.print("parse error: ${e}")
```

```text
name = witchy
```

`table_field` navigates the tree, and the `as_string` / `as_array` / `as_table`
family asserts the type at each leaf, carrying a context label into the error
message so a failure tells you *which* field was wrong. The higher-level
`required_string`, `optional_string`, and `string_array_field` helpers combine a
lookup and a type check into one call for the common cases.

In a real program the TOML text arrives from a file — read through a `Dir`
capability — but parsing it is capability-free, which keeps your config-loading
logic easy to unit-test with an inline string exactly like the examples above.
