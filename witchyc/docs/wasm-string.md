# Source UTF-8 String on Vec

`library/wasm-string.wy` defines an ordinary one-owner `String` record holding a
`Vec<U8>`. It opens with `import "wasm-vec.wy"` and performs no memory operation
of its own: every read and write goes through the Vec library's public
functions. The unchecked reads and writes underneath are the Vec library's, and
those carry Witchy-authored proofs (see [proof-required memory](wasm-proof-memory.md)).
The compiler has no rule that names String or Vec.

| Function | Meaning |
| --- | --- |
| `String.new` | Fallibly allocate an empty String. |
| `String.from_utf8` | Consume a Vec; return a validated String or the unchanged Vec. |
| `String.into_bytes` | Consume a String and return the same Vec owner. |
| `String.byte_len` | Borrowed byte count, not a Unicode scalar count. |
| `String.byte_at` | Borrowed byte lookup with a fallible U64 error. |
| `String.push_scalar` | Append the UTF-8 encoding of one Unicode scalar value. |
| `String.append` | Append a copy of another String. |
| `String.try_release` | Empty release, or return the same String owner. |
| `String.close` | Drain the storage and release the owner. |

Validation reads only. A `Vec` is well formed by its type invariant, so only
the bytes are checked: `utf8_valid` accepts
exactly the encodings of U+0000 through U+10FFFF without surrogates and rejects
overlong, truncated and out-of-range forms. A refused Vec comes back unchanged.

`String.push_scalar` refuses surrogates and values above U+10FFFF. Both append
functions can fail on allocation; on any failure they cut the vector back to
its original length with `Vec.truncate`, so a partly written scalar never
remains. Scalar-to-byte narrowing uses `U8.truncate(value)`, which keeps the
low eight bits and never traps.

## What is and is not established

Proved, by fresh kernel checks on every build: the Vec operations String calls
(`Vec.new`, `Vec.get`, `Vec.push`, `Vec.push_spare`, `Vec.take_last`, `Vec.truncate`, `Vec.reserve` and its
copy and cleanup helpers) meet their authored contracts, including the bounds
and initialization conditions of each unchecked read and write.

`String.new`, `String.into_bytes` and `String.close` carry contracts of their
own, proved from Vec's contracts: a new String is empty with a capacity that
matches its allocation, unwrapping returns the same bytes unchanged, and closing
releases the allocation. Their element type is the fixed `U8`, which the
ownership model treats as a bounded number in place of a generic parameter.

`utf8_push` and `String.push_scalar` are proved as well. On success the byte
vector is one longer (at least one longer, for a scalar) and `utf8_push` stores
the given byte at the old length; that the capacity matches the allocation is
the Vec invariant and no longer a clause. On failure `utf8_push` returns a
vector no longer than its input. Nothing is claimed yet about
`String.push_scalar`'s failure result.

`String.try_release` is proved from `Vec.try_release`: success means the
allocation was released, failure returns the same bytes and length.

`String.byte_len` returns the length header and `String.byte_at` returns the
stored byte at an index below it, or code 1; both leave the bytes unchanged.
`String.byte_at` is proved from `Vec.get`'s contract.

`utf8_valid` is proved to only read: bytes and headers are unchanged. It is not
proved to decide UTF-8 validity; the contract language has no way to state
that. `String.from_utf8` returns the same bytes and headers either way.

`String.append` is proved from `String.byte_at`, `utf8_push` and
`Vec.truncate`: on success the result is at least as long as the input, and on
every path exactly one String is returned and nothing leaks. The borrowed
suffix is opaque to the proof: the body can only hand it to read-only helpers.
Its contents after the call (that the suffix bytes were appended) are not
stated.

Not proved: UTF-8 well-formedness
is maintained by this library's code and exercised by tests; it is not a
kernel theorem or a compiler-enforced refinement. `String` and `Vec` are sealed, so only this
library can construct a String or edit Vec fields; the proofs do not yet rely on
that, and the guards that make forged contents fail at run time are still present. The checker, VC translation, allocator, layout and
Wasm emitter remain trusted as described in [the architecture](proof-architecture.md).

## Focused check

`tests/fixtures/string/build.wy` builds "hé✓🧙" scalar by scalar, appends it to
another String, round-trips the bytes through validation and confirms that a
stray continuation byte is rejected. Run it with `scripts/run-fixtures.py`
(see [development](development.md)); it requires every obligation to be freshly
kernel checked and compares real Wasmtime output.

`scripts/test-wasm-string.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md) predates the proved Vec library and this API. It
concatenates sources and expects zero obligations, so it no longer describes the
current library and needs a separate update or retirement.

## Text operations

`library/wasm-text.wy` adds methods to `String` in safe code: it uses only the
public methods above and touches no sealed field; `parse_u64` has a contract whose proof
shows its arithmetic cannot overflow.

| Method | Result |
|---|---|
| `equals_text(text)`, `starts_with_text(text)` | compare with a literal |
| `compare(&other)` | `Less`, `Equal` or `Greater`, in byte order (which is scalar order for UTF-8) |
| `less(&other)` | `Bool` |
| `find_byte(byte, from)` | `Ok(index)` or `Err(1)` |
| `slice(allocator, from, to)` | a new String of bytes `from..to`; `Err(2)` when an end is past the text or inside a scalar, `Err(1)` when there is no room |
| `push_slice(allocator, &source, from, to)` | the same, appended in place; `Result<(), U64>` |
| `parse_u64()` | `Ok(number)`; `Err(1)` not digits, `Err(2)` too large |
| `push_u64(allocator, number)` | decimal digits appended in place; `Result<(), U64>` |

`String.cut(length)` (in `wasm-string.wy`) shortens in place and, like
`truncate`, does nothing when `length` falls inside a scalar.

`slice` copies scalar by scalar through `String.push`, so the result is
well-formed for the same reason any pushed text is. Nothing new is proved or
trusted.

`tests/fixtures/text-ops/operations.wy` exercises each method, including the
largest `U64`, one more than it, and a slice that would cut `ä` in half.
`tests/fixtures/programs/frequency.wy` reads a file, cuts lines into words,
counts them in a `HashMap<String, U64>` and prints them by count and then by
word; its expected output was computed separately.
