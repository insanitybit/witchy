# Bytes and Binary Data

`String` in witchy is text - a sequence of Unicode characters. When you need
raw octets - a file's contents, a network frame, a hash digest - you want
`Bytes`, the binary counterpart. The `bytes` module builds and inspects them,
`ascii` classifies byte-sized characters, and `encoding` (from the text chapter)
moves bytes through hex and base64.

## Building and slicing

`bytes.from_string` converts text to its UTF-8 octets; `to_string_lossy` goes
back (replacing any invalid sequence rather than failing). `length`, `at`,
`slice`, and `concat` work like their `list` counterparts:

```witchy
import bytes
import ascii

fn main(console: Console):
    let b = bytes.from_string("Witchy!")
    console.print("length: ${b.length()}")
    console.print("first byte: ${b.at(0)}")
    // Slice and concatenate.
    let head = b.slice(0, 3)
    console.print("head: ${head.to_string_lossy()}")
    // Inspect bytes with ascii classification.
    var letters = 0
    for i in list.range_between(0, b.length()):
        let ch = b.slice(i, i + 1).to_string_lossy()
        if ascii.is_alpha(ch):
            letters = letters + 1
    console.print("letters: ${letters}")
```

```text
length: 7
first byte: 87
head: Wit
letters: 6
```

The first byte of `"Witchy!"` is `87` - the ASCII code for `W`. The `ascii`
module (`is_alpha`, `is_digit`, `is_space`, `to_digit`, …) classifies single
characters, which is exactly what you want when walking bytes one at a time.

## From raw values, with validation

To construct bytes from numbers directly, `bytes.from_list_string` takes a
`List(Int)` and returns a `Result` - because a byte must be in `0..=255`, an
out-of-range value is a clean error rather than a silent truncation:

```witchy
import bytes
import encoding

fn main(console: Console):
    // Build bytes from raw values, then hex-encode them.
    match bytes.from_list_string([72, 105, 33]):
        Ok(b) ->
            console.print("decoded text: ${b.to_string_lossy()}")
            console.print("as hex: ${encoding.hex_encode_bytes(b)}")
        Err(e) -> console.print("bad bytes: ${e}")
    // A byte value out of range surfaces as an error, not a silent wrap.
    match bytes.from_list_string([256]):
        Ok(_) -> console.print("unexpectedly ok")
        Err(e) -> console.print("rejected: ${e}")
```

```text
decoded text: Hi!
as hex: 486921
rejected: bytes.from_list: value 256 is outside 0..=255
```

That rejection is the pattern to notice: witchy's numeric types don't wrap
silently at a byte boundary, and the library won't pretend `256` is a byte.
The error tells you the offending value and the valid range.

## When to reach for `Bytes`

Use `String` for anything textual - you get interpolation, the `string`
methods, and correct Unicode handling. Reach for `Bytes` at the binary
boundary: reading a non-text file, hashing (a digest is bytes, rendered as hex),
constructing a wire format, or handling data that might not be valid UTF-8. The
`decode_utf8_string` method is the safe bridge back - it returns a `Result`, so
you decide what to do with malformed input instead of getting a silent
replacement character. Keep the two types distinct and the compiler keeps your
text-vs-binary confusion at compile time.
