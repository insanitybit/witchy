# Text literals and formatting

## `&str` and `Text`

```
let label = "n="          // &str
label.len()               // U64, as label.byte_len()
label.byte_at(0)          // Result<U8, U64>
let names = ["a", "b"]    // [Text; 2]: an array stores its literals as Text
console.line(names[0])    // a Text is given as a &str wherever one is expected
```

A text literal is borrowed UTF-8, `&str`: an address and a length into the program's static
bytes (read-only data on native, the literal region between the stack and the heap on Wasm).
It needs no allocator, and every library function that reads text takes `&str`: a literal, a
`String`'s `as_str()`, and a checked `str.from_utf8_bytes(bytes)` are all the same to it.

A `&str` is a reference, so it lives in locals and is never stored. Where a literal has to be
stored (an array's element, a record's field, a map key, a test's name) it is a `Text`: the
literal's number in the program's table, Copy, eight bytes. `src/parser/text_literal.rs`
writes `Text` as ordinary Witchy source: a record holding that number, and `as_str`, which
finds the literal by halving and returns its static bytes. A `Text` is turned into a `&str`
by the checker wherever a `&str` is expected, and has `str`'s methods (`label.byte_at(0)`).
A literal is a `Text` where the type asked for is one (`let t: Text = "x"`, a `Text` field),
in an array with no type written, and under `&` (`map.get(&"pen")`, a `&F` given to a
generic `F`); everywhere else it is a `&str`.

The same text written twice is one literal. At most 4096 different literals per program; on
Wasm, all of a program's literal bytes fit the region below the heap (31,744 bytes in a heap
module; the largest program in the workspace uses 4,793). A `Text` whose number names no
literal reads as empty.

## `Show`

`library/wasm-format.wy`:

```
trait Show:
    fn show(self: &Self, out: String, allocator: &Allocator) -> Result<String, String>
```

`show` appends the value's text to `out`. `Err` returns `out` as it was given:
a failed append is cut back with `String.truncate`. Implemented for `str`, `Text`,
`String`, `U64` and `Bool`; a program implements it for its own types.
`tests/fixtures/format/show.wy` builds `n=1205, é 0true`.

The library is plain client code of `String`: no proofs and no unchecked
operation. `str.show` and `Text.show` append the bytes in one copy through
`String.push_text`, whose `&str` input already guarantees UTF-8.

`String.truncate(length)` is new: it cuts the text back to `length` bytes and
refuses (returns the text whole) when `length` falls inside a scalar's encoding.

## Not yet

- A method that borrows its receiver needs a named value, so `"true".show(...)`
  must be written with a `let`.
- Interpolation (`"n=${n}"`) is described in docs/design-format.md; this page's
  `show` methods append pieces one at a time.

## Pointer diagnostics

Use `import pointer_format` to use `pointer_format.write_weak`.
A direct build with `--profile=debug` writes the weak pointer's address bits
in hexadecimal. On native this is a slot number. On Wasm it is a linear-memory offset. Release builds, including builds with no profile flag,
write `0x0000`. The release branch never reads the bits. Low-level operations
such as `memory.weak_bits` are separate primitives, including for code that
checks whether a weak link is null.


## Binary encodings

`import encoding` provides padded base64, unpadded base64url, and hex.
The encoders append ASCII to a `String`. The decoders return owned
`Vec<U8>` values, so the decoded bytes can contain any values.

`push_base64url` uses `-` and `_` and omits padding. Its decoder rejects
padding, whitespace, the standard alphabet's `+` and `/`, and nonzero
unused bits in the last symbol. `InvalidBase64(position)` identifies the
zero-based byte position, or the input length when the final group is
incomplete. `Base64Allocation(code)` reports an allocator refusal.
[RFC 4648 sections 3.5 and 5](https://www.rfc-editor.org/rfc/rfc4648.html#section-3.5)
and [the unpadded format used by JWS](https://www.rfc-editor.org/rfc/rfc7515.html#section-2).

The padded `decode_base64` retains its original error codes and accepts
extra bits in the last padded group.
[Encoding API reference](library-text.md#import-encoding-librarywasm-encodingwy)
