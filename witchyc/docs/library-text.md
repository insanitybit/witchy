# Text, numbers as text, console output and encodings: library reference

Generated from the library source by `scripts/library-reference.py`; each line is a public
function as declared, with the first sentence of its comment. `self: &String` is called as
`text.name(...)`; a function with no `self` is called as `name(...)`.

## `import string` (library/wasm-string.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `String.new` | `(allocator: &Allocator) -> Result<String, U64>` | An empty String. |
| `String.with_capacity` | `(allocator: &Allocator, capacity: U64) -> Result<String, U64>` | An empty String with room for `capacity` bytes, so text of a known length is one allocation (`new` then appending reserved again on the first append). |
| `String.from_str` | `(allocator: &Allocator, source: &str) -> Result<String, U64>` | Own a copy of text whose source already guarantees valid UTF-8. |
| `String.from_utf8_bytes` | `(allocator: &Allocator, bytes: &[U8]) -> Result<String, U64>` | Own a copy of validated UTF-8. |
| `String.repeated_ascii` | `(allocator: &Allocator, byte: U8, count: U64) -> Result<String, U64>` | `count` copies of one ASCII byte, made in one fill. |
| `String.from_utf8` | `(input: Vec<U8>) -> Result<String, Vec<U8>>` | Consume a Vec; return a String owning the same storage, or the unchanged Vec. |
| `String.into_bytes` | `(input: String) -> Vec<U8>` | The bytes, as the Vec that held them. |
| `String.word_at` | `(text: &String, from: U64, count: U64) -> U64` | Bytes `from..from + count` (at most eight) as one number, lowest byte first; a byte past `count` or past the end reads as zero. |
| `String.byte_len` | `(value: &String) -> U64` | How many bytes the text is (not how many scalars: `scalar_count`). |
| `String.byte_at` | `(value: &String, index: U64) -> Result<U8, U64>` | The byte at `index`. |
| `String.push_scalar` | `(input: String, allocator: &Allocator, scalar: U64) -> Result<String, String>` | Append the UTF-8 encoding of one Unicode scalar value. |
| `String.append` | `(input: String, allocator: &Allocator, suffix: &String) -> Result<String, String>` | Append a copy of `suffix`. |
| `String.scalar_at` | `(value: &String, index: U64) -> Result<Scalar, U64>` | The scalar whose encoding starts at byte `index`, and how many bytes it takes. |
| `String.truncate` | `(input: String, length: U64) -> String` | Cut the text back to `length` bytes. |
| `String.close` | `(input: String) -> ()` | Release the text and its room. |
| `String.try_release` | `(input: String) -> Result<(), String>` | Release an empty text. |
| `String.push` | `(self: &mut String, allocator: &Allocator, scalar: U64) -> Result<(), U64>` | Append one Unicode scalar value in place. |
| `String.reserve` | `(self: &mut String, allocator: &Allocator, more: U64) -> Result<(), U64>` | Make room for `more` further bytes, at least doubling when it grows. |
| `String.clear` | `(self: &mut String) -> ()` | Empty the text in place, keeping its room. |
| `String.cut` | `(self: &mut String, length: U64) -> ()` | Cut the text back to `length` bytes in place. |
| `String.push_ascii` | `(self: &mut String, allocator: &Allocator, word: U64, count: U64) -> Result<(), U64>` | Append the first `count` bytes of `word` (at most eight, lowest first), each ASCII, in one write. |
| `String.push_decimal` | `(self: &mut String, allocator: &Allocator, number: U64) -> Result<(), U64>` | Append the decimal digits using a bounded stack scratch space. |
| `String.push_text` | `(self: &mut String, allocator: &Allocator, source: &str) -> Result<(), U64>` | Append borrowed UTF-8 in one copy. |
| `String.push_part` | `(self: &mut String, allocator: &Allocator, source: &String, from: U64, to: U64) -> Result<(), U64>` | Append bytes `from..to` of `source`, copied as they are: each end must be where a scalar starts, or the end of the text, so what is copied is whole scalars. |
| `String.push_string` | `(self: &mut String, allocator: &Allocator, suffix: &String) -> Result<(), U64>` | Append a copy of another String. |
| `String.push_utf8_slice` | `(self: &mut String, allocator: &Allocator, bytes: &[U8]) -> Result<(), U64>` | Validate borrowed bytes before appending them directly. |
| `String.as_bytes` | `(text: &String) -> &[U8]` | The UTF-8 bytes as a slice (docs/design-slices.md): no copy; the String stays lent while the slice is in use. |
| `String.as_str` | `(text: &String) -> &str` | Borrow the proved initialized UTF-8 prefix without scanning or copying. |

## `import text` (library/wasm-text.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `find_bytes` | `(bytes: &[U8], pattern: &[U8], from: U64) -> Result<U64, U64>` | First byte offset at or after from, or Err(1) when no match exists. |
| `String.byte_or_end` | `(self: &String, index: U64) -> U64` | The byte at `index` as a number, or 256 past the end. |
| `String.equals_text` | `(self: &String, text: &str) -> Bool` | Whether the text is exactly these bytes. |
| `String.starts_with_text` | `(self: &String, text: &str) -> Bool` | Whether the text begins with these bytes. |
| `String.next` | `(self: &String, cursor: &mut U64) -> Result<U64, U64>` | For `for scalar in &text:`. |
| `String.compare` | `(self: &String, other: &String) -> Order` | Byte order, which for UTF-8 is the order of the scalars. |
| `String.less` | `(self: &String, other: &String) -> Bool` | Whether this text comes before the other in byte order. |
| `String.find_byte` | `(self: &String, byte: U8, from: U64) -> Result<U64, U64>` | Where `byte` first appears at or after `from`. |
| `String.find_text` | `(self: &String, needle: &str, from: U64) -> Result<U64, U64>` | Search for borrowed UTF-8 starting at a byte offset. |
| `String.replace_text` | `(self: &String, allocator: &Allocator, needle: &str, replacement: &str) -> Result<String, U64>` | A copy with every `needle` replaced by `with`, left to right, matches not overlapping: "aaa" with "aa" -> "b" is "ba". |
| `String.push_slice` | `(self: &mut String, allocator: &Allocator, source: &String, from: U64, to: U64) -> Result<(), U64>` | Append the scalars whose encodings lie in bytes `from..to` of `source`. |
| `String.slice` | `(self: &String, allocator: &Allocator, from: U64, to: U64) -> Result<String, U64>` | A new String holding bytes `from..to` of this one. |
| `String.field` | `(self: &String, allocator: &Allocator, separator: U8, cursor: &mut U64) -> Result<String, U64>` | The field that starts at byte `cursor`: the text up to the next `separator`, or to the end, as a new String. |
| `String.parse_u64` | `(self: &String) -> Result<U64, U64>` | The number written in decimal digits. |
| `String.push_u64` | `(self: &mut String, allocator: &Allocator, number: U64) -> Result<(), U64>` | Append a number in decimal. |
| `String.to_ascii_lower` | `(self: &String, allocator: &Allocator) -> Result<String, U64>` | A copy with the ASCII letters lower case; every other scalar as it is. |
| `String.to_ascii_upper` | `(self: &String, allocator: &Allocator) -> Result<String, U64>` | A copy with the ASCII letters upper case; every other scalar as it is. |
| `String.scalar_count` | `(self: &String) -> U64` | How many scalars (characters) the text holds: the bytes that are not a continuation (10xxxxxx). |
| `String.push_repeated` | `(self: &mut String, allocator: &Allocator, scalar: U64, count: U64) -> Result<(), U64>` | Append `scalar` `count` times; ASCII eight to a word. |
| `String.pad_to` | `(self: &mut String, allocator: &Allocator, width: U64, scalar: U64) -> Result<(), U64>` | Append `scalar` until the text is `width` scalars long (nothing when it is already): `line.pad_to(&allocator, 12, ' ')` fills to column 12. |
| `join_texts` | `(items: &Vec<String>, allocator: &Allocator, separator: &str) -> Result<String, U64>` | The texts in order with `separator` between each two, as one new String: ["a", "b", "c"] with ", " is "a, b, c"; none is "". |

## `import span` (library/wasm-span.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `String.span` | `(self: &String) -> Span` | The whole text as a Span. |
| `Span.of` | `(bytes: &[U8]) -> Span` | A Span of bytes that did not come from a String (bytes read from a socket or a file): nothing here assumes UTF-8 but `to_string`, which checks it. |
| `Span.len` | `(self: Span) -> U64` | How many bytes the view is. |
| `Span.byte_len` | `(self: Span) -> U64` | The same, by the name a String and a Text give their length. |
| `Span.is_empty` | `(self: Span) -> Bool` | Whether the view holds no bytes. |
| `Span.as_bytes` | `(self: Span) -> &[U8]` | The bytes themselves. |
| `Span.at` | `(self: Span, index: U64) -> Result<&U8, U64>` | The byte at `index`, as `for b in view:` asks for it (src/compiler/iteration.rs). |
| `Span.byte_or_end` | `(self: Span, index: U64) -> U64` | The byte at `index` as a number, or 256 past the end. |
| `Span.part` | `(self: Span, from: U64, to: U64) -> Span` | Bytes `from..to` of this Span; an end past the Span is its end. |
| `Span.find_byte` | `(self: Span, byte: U8) -> Result<U64, U64>` | Where `byte` first appears. |
| `Span.find_text` | `(self: Span, needle: &str, from: U64) -> Result<U64, U64>` | Where `needle` first appears at or after `from`, as a byte position in this Span. |
| `Span.split` | `(self: Span, byte: U8) -> Result<Halves, Span>` | The parts before and after the first `byte`. |
| `Span.words` | `(self: Span) -> Words` | The words, split at runs of spaces, newlines and tabs: `for word in text.span().words():`. |
| `Span.lines` | `(self: Span) -> Lines` | The lines: `for line in text.span().lines():`. |
| `Span.starts_with_text` | `(self: Span, text: &str) -> Bool` | Whether the first bytes are this literal's. |
| `Span.ends_with_text` | `(self: Span, text: &str) -> Bool` | Whether the last bytes are this literal's. |
| `Span.eq_ignore_ascii_case` | `(self: Span, other: Span) -> Bool` | Whether two Spans hold the same bytes once ASCII letters are taken as one case. |
| `Span.as_str` | `(self: Span) -> Result<&str, U64>` | Borrow the same bytes as text after checking UTF-8. |
| `Span.is_utf8` | `(self: Span) -> Bool` | Whether the bytes are well-formed UTF-8. |
| `Span.equals_text_ignore_ascii_case` | `(self: Span, text: &str) -> Bool` | Whether the bytes are these once ASCII letters are taken as one case: a header name against `"content-length"`, with no copy of the text made. |
| `Span.contains_text` | `(self: Span, text: &str) -> Bool` | Whether the literal appears anywhere. |
| `Span.trim` | `(self: Span) -> Span` | Without the spaces at either end. |
| `Span.equals_text` | `(self: Span, text: &str) -> Bool` | Whether the bytes are exactly these. |
| `Span.same` | `(self: Span, other: Span) -> Bool` | Whether two Spans hold the same bytes. |
| `Span.parse_u64` | `(self: Span) -> Result<U64, U64>` | The number written in decimal digits. |
| `Span.to_string` | `(self: Span, allocator: &Allocator) -> Result<String, U64>` | A String of its own holding these bytes. |
| `write_span` | `(console: &Console<Write>, span: Span) -> ()` | The bytes of a Span, as they are. |
| `find_by_span` | `<V: Any>(map: &HashMap<String, V>, key: Span) -> Result<U64, U64>` | A map keyed by String looked up with a part of a text, without copying it into a String: `get_by_span(&counts, word)`. |
| `contains_span` | `<V: Any>(map: &HashMap<String, V>, key: Span) -> Bool` | Whether a map keyed by String has a key with these bytes, without copying them into a String. |
| `get_by_span` | `<V: Any>(map: &HashMap<String, V>, key: Span) -> Result<&V, U64>` | The value of the key with these bytes. |
| `get_mut_by_span` | `<V: Any>(map: &mut HashMap<String, V>, key: Span) -> Result<&mut V, U64>` | The value of the key with these bytes, to change in place. |
| `Words.next` | `(self: &Words, cursor: &mut U64) -> Result<Span, U64>` | The next word from byte `cursor` on; the cursor moves past it. |
| `Lines.next` | `(self: &Lines, cursor: &mut U64) -> Result<Span, U64>` | The next line from byte `cursor` on; the cursor moves past its newline. |
| `Span.parse_i64` | `(self: Span) -> Result<I64, U64>` | As `String.parse_i64` (library/wasm-int.wy), over a view. |

## `import console` (library/wasm-console.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `WordWriter.new` | `() -> WordWriter` | A writer holding no bytes yet. |
| `WordWriter.put` | `(self: &mut WordWriter, console: &Console<Write>, byte: U8) -> ()` | Add a byte; eight make one write. |
| `WordWriter.flush` | `(self: &mut WordWriter, console: &Console<Write>) -> ()` | Write what is held and start again. |
| `Console<Write>.write` | `(console: &Console<Write>, text: &str) -> ()` | Write borrowed UTF-8 without allocating. |
| `Console<Write>.write_bytes` | `(console: &Console<Write>, bytes: &[U8]) -> ()` | Write the whole borrowed byte slice in order. |
| `Console<Write>.line` | `(console: &Console<Write>, text: &str) -> ()` | Write a text and a newline. |
| `Console<Write>.write_repeated` | `(console: &Console<Write>, byte: U8, count: U64) -> ()` | The first `count` bytes of `word` (fewer than eight), lowest first. |
| `Console<Write>.write_part` | `(console: &Console<Write>, word: U64, count: U64) -> ()` | Write the low `count` bytes of `word`, lowest first. |
| `Console<Write>.write_string` | `(console: &Console<Write>, text: &String) -> ()` | Write a String. |
| `Console<Write>.write_u64` | `(console: &Console<Write>, number: U64) -> ()` | A number in decimal, with no newline after it. |
| `Console<Write>.write_spaces` | `(console: &Console<Write>, count: U64) -> ()` | Write `count` spaces. |
| `Console<Write>.write_u64_right` | `(console: &Console<Write>, number: U64, width: U64) -> ()` | A number at the right of a column `width` wide. |
| `Console<Write>.write_string_left` | `(console: &Console<Write>, text: &String, width: U64) -> ()` | A String at the left of a column `width` bytes wide. |
| `Console<Read>.read_byte` | `(input: &Console<Read>) -> Result<U8, U64>` | The next byte of input. |
| `Console<Read>.continuation` | `(input: &Console<Read>) -> U64` | The low six bits of the next byte, or 256 if it does not continue a scalar's encoding. |
| `Console<Read>.read_line` | `(input: &Console<Read>, allocator: &Allocator, line: &mut String) -> .[Line \| End \| NotText \| NoRoom]` | Append the next line of input to `line`, without its newline. |
| `push_u64` | `(text: &mut String, allocator: &Allocator, number: U64) -> Result<(), U64>` | Appends the decimal digits of `number`. |
| `Console<Write>.show` | `<T: Show>(console: &Console<Write>, value: T) -> ()` | One value, by its `Show`. |
| `Console<Write>.print` | `<F: Format>(console: &Console<Write>, message: F) -> ()` | A format and the end of the line: `console.print("total ${t}")`. |
| `Console<Write>.write_format` | `<F: Format>(console: &Console<Write>, message: F) -> ()` | A format with no end of line: `console.write_format("${word}:")`. |
| `String.push_format` | `<F: Format>(self: &mut String, allocator: &Allocator, message: F) -> Result<(), U64>` | Appends a format's text. |

## `import int` (library/wasm-int.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `I64.from` | `(value: U64) -> Result<I64, U64>` | The I64 equal to `value`. |
| `I64.negative` | `(self: I64) -> Bool` | Whether it is below zero. |
| `I64.magnitude` | `(self: I64) -> U64` | The distance from zero. |
| `I64.below_zero` | `(magnitude: U64) -> Result<I64, U64>` | The I64 that far below zero. |
| `I64.to_u64` | `(self: I64) -> Result<U64, U64>` | The value as a U64. |
| `I64.least` | `(self: I64, other: I64) -> I64` | The lesser of the two; the same as `min`. |
| `I64.greatest` | `(self: I64, other: I64) -> I64` | The greater of the two; the same as `max`. |
| `I64.checked_add` | `(self: I64, other: I64) -> Result<I64, U64>` | Arithmetic that answers instead of trapping. |
| `I64.checked_sub` | `(self: I64, other: I64) -> Result<I64, U64>` | self - other. |
| `I64.checked_mul` | `(self: I64, other: I64) -> Result<I64, U64>` | self * other. |
| `I64.checked_div` | `(self: I64, other: I64) -> Result<I64, U64>` | self / other, toward zero. |
| `I64.checked_rem` | `(self: I64, other: I64) -> Result<I64, U64>` | The remainder of `checked_div`, with the sign of self. |
| `I64.checked_neg` | `(self: I64) -> Result<I64, U64>` | -self. |
| `Console<Write>.write_i64` | `(console: &Console<Write>, value: I64) -> ()` | The number in decimal, with `-` in front when it is below zero. |
| `Console<Write>.print_i64` | `(console: &Console<Write>, value: I64) -> ()` | The same and a newline. |
| `String.push_i64` | `(self: &mut String, allocator: &Allocator, number: I64) -> Result<(), U64>` | Append a number in decimal. |
| `String.parse_i64` | `(self: &String) -> Result<I64, U64>` | The number written as decimal digits, with `-` first when it is below zero. |
| `U64.min` | `(self: U64, other: U64) -> U64` | The lesser of the two. |
| `U64.max` | `(self: U64, other: U64) -> U64` | The greater of the two. |
| `U64.digit_count` | `(self: U64) -> U64` | How many decimal digits it is written with (1 for zero): its width in a column. |
| `U64.clamp` | `(self: U64, low: U64, high: U64) -> U64` | `low` if below it, `high` if above it (for `low` not above `high`; otherwise `low`). |
| `I64.min` | `(self: I64, other: I64) -> I64` | The lesser of the two. |
| `I64.max` | `(self: I64, other: I64) -> I64` | The greater of the two. |
| `I64.clamp` | `(self: I64, low: I64, high: I64) -> I64` | As `U64.clamp`. |

## `import float` (library/wasm-float.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `Console<Write>.write_f64` | `(console: &Console<Write>, value: F64, decimals: U64) -> ()` | `value` rounded to `decimals` places (at most 9), exactly, half to even, with `-` in front when its sign is negative (`-0.00` too, as printf writes it). |
| `Console<Write>.print_f64` | `(console: &Console<Write>, value: F64, decimals: U64) -> ()` | The same and a newline. |
| `parse_f64` | `(bytes: &[U8]) -> Result<F64, U64>` | The decimal number in `bytes`: an optional sign, digits with an optional `.`, an optional exponent (`e` or `E`, a sign, digits). |
| `String.parse_f64` | `(self: &String) -> Result<F64, U64>` | The F64 this text writes, as `parse_f64` reads it. |
| `String.push_f64` | `(self: &mut String, allocator: &Allocator, value: F64) -> Result<(), U64>` | Append `value` as the shortest decimal that reads back as it, as Python's `repr` writes it. |
| `F64.min` | `(self: F64, other: F64) -> F64` | The lesser of the two; NaN if either is, -0.0 below 0.0. |
| `F64.max` | `(self: F64, other: F64) -> F64` | The greater of the two; NaN if either is, 0.0 above -0.0. |

## `import encoding` (library/wasm-encoding.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `push_base64` | `(text: &mut String, allocator: &Allocator, bytes: &[U8]) -> Result<(), U64>` | Append `bytes` in base64. |
| `push_base64url` | `(text: &mut String, allocator: &Allocator, bytes: &[U8]) -> Result<(), U64>` | Append bytes as unpadded base64url. |
| `decode_base64` | `(allocator: &Allocator, text: &[U8]) -> Result<Vec<U8>, U64>` | The bytes base64 `text` encodes. |
| `decode_base64url` | `(allocator: &Allocator, text: &[U8]) -> Result<Vec<U8>, Base64Error>` | Decode canonical unpadded base64url into owned bytes. |
| `decode_base64url_bounded` | `(allocator: &Allocator, text: &[U8], max_bytes: U64) -> Result<Vec<U8>, BoundedBase64Error>` | Refuse an output above max_bytes before allocating. |
| `push_hex` | `(text: &mut String, allocator: &Allocator, bytes: &[U8]) -> Result<(), U64>` | Append `bytes` as hex, two lowercase digits each. |
| `decode_hex` | `(allocator: &Allocator, text: &[U8]) -> Result<Vec<U8>, U64>` | The bytes hex `text` writes, two digits a byte, either case. |

