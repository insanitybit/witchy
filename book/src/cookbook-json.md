# Working with JSON

JSON is the format most programs exchange with the outside world, so witchy
makes both directions first-class. Encoding is *reflective*: any type that
derives `Reflect` serializes with no per-type code. Decoding is *typed*: you
parse a string into a `Json` value and either navigate it by hand or let a
derived `Deserialize` pull it into your own record.

## Encoding: `json.stringify`

Give a type `derive(Reflect)` (which needs `import reflect`) and
`json.stringify` turns any value of it — and any list, option, or nesting of
them — into a JSON string. There is no `derive(Json)`; serialization rides on
reflection.

```witchy
import json
import reflect

type Point derive(Reflect):
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(3, 4)
    // Any Reflect type stringifies straight to JSON.
    console.print(json.stringify(p))
    console.print(json.stringify([1, 2, 3]))
```

```text
{"x":3,"y":4}
[1,2,3]
```

`json.from_value(x)` does the same but returns a `Json` value instead of a
string, when you want to embed it in a larger structure before encoding.

## Decoding: parse then navigate

`json.decode` returns `Result(Json, DecodeError)` — a parse can fail, so you
handle that up front. The resulting `Json` is a sum type you inspect with `get`
(object field), `index` (array element), and the `as_*` accessors, each
returning an `Option` because the value might not be the shape you asked for.

```witchy
import json

fn main(console: Console):
    let src = "{\"name\": \"ada\", \"scores\": [90, 85, 88]}"
    match json.decode(src):
        Ok(doc) ->
            match doc.get("name"):
                Some(n) ->
                    match n.as_string():
                        Some(s) -> console.print("name: ${s}")
                        None -> console.print("name not a string")
                None -> console.print("no name")
            match doc.get("scores"):
                Some(arr) ->
                    match arr.as_array():
                        Some(xs) -> console.print("scores: ${xs.length()}")
                        None -> console.print("scores not an array")
                None -> console.print("no scores")
        Err(e) -> console.print("bad json: ${e}")
```

```text
name: ada
scores: 3
```

For a few fields this hand-navigation is fine. When you want the whole document
validated into a typed record, though, the `require` / `int_of` / `string_of`
family returns a `Result` with a structured `DeserializeError` you can thread
with `?` — no cascade of `Option` matches.

## Decoding into your own type: `derive(Deserialize)`

The concise path is `derive(Deserialize)`, which generates a `from_json` method
that pulls a `Json` value straight into your record, checking each field's type.
Decode the string to a `Json` first, then deserialize:

```witchy
import json
import reflect

type Config derive(Reflect, Deserialize):
    name: String
    port: Int

fn main(console: Console):
    let src = "{\"name\": \"api\", \"port\": 8080}"
    match json.decode(src):
        Ok(doc) ->
            match Config.from_json(doc):
                Ok(c) -> console.print("${c.name} on port ${c.port}")
                Err(e) -> console.print("bad config: ${e}")
        Err(e) -> console.print("invalid json: ${e}")
```

```text
api on port 8080
```

Note the two-step shape — `json.decode` (is it valid JSON?) then `from_json` (is
it the *right* JSON?). Each stage has its own error type, so a malformed payload
and a well-formed-but-wrong payload are distinguishable. Derive `Reflect` too if
you also want to serialize the type back out; the two derives are independent and
compose.
