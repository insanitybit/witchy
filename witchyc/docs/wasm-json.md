# JSON

Use `import json` to parse JSON from a `String` and send events to a sink the
program supplies. The parser uses the allocator passed alongside the input.
The event parser and tree API run on Wasm and macOS ARM64.
[Event fixture](../tests/fixtures/json/events.wy) ·
[Round-trip fixture](../tests/fixtures/json/roundtrip.wy)

```
trait JsonSink:
    fn null(self: &mut Self)
    fn boolean(self: &mut Self, value: Bool)
    fn number(self: &mut Self, source: &String, start: U64, end: U64)
    fn string(self: &mut Self, text: &String)
    fn key(self: &mut Self, text: &String)
    fn begin_array(self: &mut Self)   fn end_array(self: &mut Self)
    fn begin_object(self: &mut Self)  fn end_object(self: &mut Self)

json_parse(&input, &mut sink, &allocator)   // Result<(), U64>
```

The parser accepts the JSON grammar: whitespace, `null`, `true`, `false`,
numbers with sign, fraction and exponent, strings with every escape including
`\uXXXX` and surrogate pairs, arrays and objects, to a depth of 32. Strings and
keys are decoded into a `String` that the sink sees by reference and the parser
closes. A number is handed over as its span in the input, so a reader chooses how to
read it; `json_u64` reads a span of plain digits.

Errors are codes: 1 input ended early, 2 unexpected byte, 3 bad escape,
4 nested too deeply, 5 allocation refused, 6 text after the value. On any error
every `String` the parser made has been closed.

It is plain Witchy code on `String`'s public functions, generic over the sink
through a trait: no proofs, nothing unchecked. `tests/fixtures/json/events.wy`
tallies ten inputs, six of them malformed.

`String.scalar_at(index)` is new: the scalar that starts at a byte index and
its width, for walking a `String` scalar by scalar.

## The tree

Use `import json_tree` to build an owned document.

```
type Json:
    Null
    Boolean(Bool)
    Whole(U64)          // plain digits that fit U64
    Decimal(String)     // any other number, as written
    Str(String)
    Array(Vec<Json>)
    Object(Vec<Member>)

type Member:
    key: String
    value: Json

json_tree(&input, &allocator)   // Result<Json, U64>
document.discard()
```

`Json` holds itself inside `Vec`, which `docs/wasm-sums.md` describes. The
builder is recursive descent over the same scanning functions as the event
parser, with the same error codes; on an error every part already built is
closed. `tests/fixtures/json/tree.wy` parses five inputs, three malformed, and
takes each document apart.

`Json` implements `Show`: `document.show(move out, &allocator)` appends compact
JSON text, reading the document through a reference and escaping strings as
JSON requires. `tests/fixtures/json/roundtrip.wy` and `roundtrip-escapes.wy`
parse text, write the document back twice, and compare with the expected text.

## Writing strings

Use `import json_strings` and
`append_json_string(&mut output, &allocator, &text, encoding)` to append
one quoted string. Select `JsonUtf8` to keep valid non-ASCII UTF-8 or
`JsonAscii` to escape it. Both modes escape quotes, backslashes, and
control bytes. ASCII mode writes non-BMP scalars as pairs of Unicode
escapes and escapes DEL. If writing fails, the output retains its
original bytes.

`Json.show` uses UTF-8 mode. Coven uses ASCII mode for release receipts
and keeps its signature-specific key ordering and number rules in
the package.

## Unique object keys

Use `import json_sorted` and `json_sorted_tree(&input, &allocator)` for
documents whose object keys must be unambiguous. It rejects duplicate
decoded keys at any depth, including different spellings such as `"a"`
and `"\u0061"`. Error 11 means a duplicate key. Other errors match
`json_tree`.

Object keys are sorted by UTF-8 byte order and array order is preserved.
Use the ordinary `json_tree` when the source member order matters.
The sorted parser leaves number representation and output escaping to
the serializer. Coven uses it before producing its protocol-specific
signature bytes.

## Cost at run time

The host stops a module after 1,000,000 Wasm instructions. An access no
longer costs time in proportion to the buffer's size (see
`docs/wasm-runtime-cost.md`), but a buffer operation is still a few hundred
instructions, so these programs stay modest.
