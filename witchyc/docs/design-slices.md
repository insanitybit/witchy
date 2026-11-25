# Slices: `&[T]`

Status: decided 2026-11-16 (docs/decisions.md); arrays, Vecs, Strings and record fields
built; `View<T>` removed; `Span` rebuilt over `&[U8]`; `&mut [T]` built.

A function that takes a borrowed run of elements should not care which container holds
them. `&[T]` is that type: a reference to a run of initialized elements, independent of
whether they live in a fixed array, a Vec or a String.

## What it is

- `[T]` is a type only behind a reference. `&[T]` is two words, the address of the first
  element (as a `&T`) and the count (layout.rs `flat`).
- It borrows like `&T`: the loan of whatever it was made from stays taken while the slice,
  a part of it, or a reference read from it is in use. It points at stored data, never at a
  value holding a reference, so none of the references-to-views rules apply.
- Elements are stored values (no references, no slices of views).

## Operations (built)

| | |
|---|---|
| `a.slice()` | the whole fixed array `a` as `&[T]`; takes the array's loan |
| `s.len()` | the count |
| `s.at(i)` | `Result<&T, U64>`: `Ok` with a reference, `Err(i)` past the end (as a Vec's `at`) |
| `s[i]` | a copy of element `i` (Copy elements); past the end traps |
| `for item in s` | each element by reference, through `at` (docs/wasm-iteration.md) |
| `s.part(from, to)` | elements `from..to`; `from > to` or `to > len` traps |
| `s.part_proved(from, to)` | the same part where the function's proof shows `from <= to <= s.len()`: no range test, no trap, so a `nopanic fn` may take it (`slices/part-proved.wy`). Without the proof it keeps the test |
| `v.as_slice()` | a Vec's elements (library/wasm-vec.wy), proved from its invariant |
| `text.as_bytes()` | a String's UTF-8 bytes as `&[U8]` (library/wasm-string.wy) |
| `memory.slice_unchecked(&buffer, from, to)` | cells `from..to`; proved: `from ≤ to ≤ capacity` and each cell holds a value |

A slice may be a parameter, a result (`-> &[U64]`, borrowing from a reference parameter, as
any returned reference does) and a local; `fn f<T>(items: &[T])` infers `T` from the slice.
A record may hold a `&[T]` field: it is then a view, keeping what the slice borrows lent,
and in memory the field is 16 bytes, the address then the count (`field.wy`). `Span`
(library/wasm-span.wy) is now a sealed wrapper over one: `name.span()` lends a String's
bytes, and every read goes through the slice's bounds. Bounds are checked at run time, so
code using a slice needs no proof. Fixtures: `tests/fixtures/slices/`; the proof control
`past-length.refused.wy` slices a prefix-invariant buffer up to its capacity, and Lean refuses
it.

## `&mut [T]`

A unique slice lends what it was made from uniquely: `a.slice_mut()` (a named `var` array),
`v.as_mut_slice()` (a Vec, through the proved `memory.slice_mut_unchecked`, the same
precondition as the shared form). Through it:

| | |
|---|---|
| `s[i] = v`, `s[i] += v` | written as `let c = s.cell(i); *c = v`, so the ordinary rules for writing through `&mut` decide it |
| `s.cell(i)` | `&mut T` to element `i`; past the end traps |
| `s.at(i)` | `Result<&mut T, U64>` |
| `s.part(from, to)` | a unique slice of `from..to`; the slice is lent while the part is used |
| `s.copy_from(source)` | every element of `source: &[T]` written over `s`, a word at a time; lengths that differ trap. Elements are numbers (U8 to U64, I64, F64). `s` stays lent while `source` is read, so the two are never the same elements (`bulk-copy/slice-copy*.wy`) |

A unique slice is lent on, not copied: passed to a function, it is usable again once the
call's result no longer needs it, and an operation whose result holds no reference
(`len`, `s[i]`) ends its loan at once, so `s[0] + s[1]` reads it twice. It may be given
where a `&[T]` is expected. Two parts, two element references, or a shared and a unique
slice of one array are never in use together (no split operation yet). An element that is
an owner cannot be overwritten through a slice: the old value would be lost. A String
has no `as_mut_bytes`: writing bytes could break its UTF-8 invariant. Fixtures:
`slices/mut*.wy`.

## UTF-8 strings

`str` is a dynamically sized UTF-8 byte sequence. The shared reference
`&str` uses the same address-and-byte-count layout as `&[U8]` and carries
the guarantee that those bytes are valid UTF-8.

- A literal such as `"pen"` is a `&str` borrowing immutable program-lifetime
  bytes; where it must be stored it is a `Text`, which is given as a `&str`
  wherever one is expected ([text literals](wasm-text-format.md)).
- `impl str` adds methods to borrowed UTF-8, and the library implements its
  traits for it: `Hash`, `HashKey`, `Eq`, `Equivalent<String>`, `KeyFor<String>`
  and `Render` for `str` (each takes `self: &str`), `Show` and `Format` for `&str`
  (`self` by value). A `&str` is a type argument where a view such as `Span` is
  (`console.show(text)`, `"${text}"`), and `str` itself is one where the generic
  code holds it only behind `&`, as Rust's `?Sized`: `map.get(text)` looks a
  String key up with `Q = str`, copying nothing.
- `&text` of a `text: &str` is `text`: a shared reference is copied.
- `owned.as_str()` borrows a String's already-valid UTF-8 storage without
  copying or scanning it. The owner remains lent for as long as the view is used.
- `str.from_utf8_bytes(bytes)` validates a shared byte slice and returns
  a view with the same loan on success. It needs no allocator.
- `String.from_utf8_bytes(&allocator, bytes)` validates and copies a byte
  slice into an owned String. `String.from_utf8(bytes)` instead consumes a
  `Vec<U8>` and reuses its storage on success. On failure it returns the vector.
- `text.part_proved(from, to)` is the part of text whose range the proof shows. An end
  inside a UTF-8 scalar gives the empty text, not a trap: two byte tests, and no range test.
- `text.as_bytes()` lends the underlying bytes. `text.part(from, to)`
  uses byte offsets and traps if either endpoint cuts through a UTF-8 scalar
  or the range is out of bounds.
- `destination.push_text(&allocator, text)` reserves storage and appends
  the bytes with a bulk copy. Its input type supplies the UTF-8 guarantee.

On native targets the compiler interns literal bytes into read-only storage.
On Wasm the host admits one UTF-8 data segment in the region between the
stack and heap and rejects overlapping bulk host writes. The region holds up
to 31,744 bytes in heap modules and 64,512 bytes in frame-only modules. The Wasm opcode scan checks which memory instructions address.
The compiler's borrow checker and emitter enforce which addresses Witchy code
can write within program memory.

The `borrowed-str` fixtures cover returning a substring of an inferred literal
and rejecting local borrow escape, mutation under a live borrowed view of bytes
or a String, implicit conversion from raw bytes, and invalid proof claims about
append and UTF-8 refinement. All six refusal diagnostics were verified for
both the macOS ARM64 and Wasm build targets.
The mid-scalar substring fixture also stops before emitting any text on both
targets. Wasmtime reports that the endpoint lies inside a UTF-8 scalar.

## How

Logical values `SliceOf`, `SliceLen`, `SliceAt`, `SlicePart` (logical_body.rs); lowered to a
two-word aggregate and `Project` for the first two, and to IR `SliceAt`/`SlicePart`, which
emit the bounds checks and `first + index * stride` (emit.rs). A slice argument or parameter
is read as its two-word value, not as a pointer place (check.rs).

## Next

- A proved split of a unique slice into two disjoint parts (`split_at`).
- Writing an owner element, handing the old one back (as `memory.exchange` does).

### Borrowed text performance

Native macOS ARM64 comparison of the main validator snapshot `b1c1eeea`
(same validator files as main's `80e91e19`) and the ownership-proof branch at
`8216b6e9`. Both used the same Lean 4.36 ready binding and identical source for
each workload. The append workload reserves 10 MB and appends the nine-byte
literal and a newline one million times. The `str` benchmark also computes a
checksum across the resulting bytes.

| Workload | First build, main / branch | Warm build median, main / branch | Prebuilt runtime median, main / branch |
|---|---:|---:|---:|
| Append | 3.36 / 6.78 s | 86.25 / 84.79 ms | 27.90 / 28.78 ms |
| String benchmark | 3.34 / 6.80 s | 91.93 / 96.69 ms | 74.04 / 76.46 ms |

First builds used separate empty proof caches, one sample per workload and
revision. Warm builds used nine alternating pairs, and prebuilt launches used
eleven alternating pairs on a busy host. Outputs matched byte for byte. The
runtime ranges overlap. The comparison measures the current proof stack as a
whole, so it does not isolate the cost of any one contract. The roughly 3.4 s
first-build increase is the main regression to explain before integrating that
stack.

A separate empty-cache `--measure` run attributed the full String workload's
regression to the prover. The prover phase rose from 3.35 to 7.09 s, with
resident request time rising from 2.95 to 6.74 s. Goal selection grew from 261
to 313, generated Lean from 306 to 468 KB, and requests from six to nine.
Native code generation stayed around 1.6 ms.

| Empty main imports | Proof phase, main / branch | Goals, main / branch | Lean requests, main / branch |
|---|---:|---:|---:|
| `vec` | 1.47 / 4.21 s | 82 / 112 | 3 / 5 |
| `string` | 3.14 / 6.73 s | 226 / 271 | 6 / 9 |

These controls ran no application code and selected the new library obligations
anyway. The Vec import alone accounts for most of the observed increase.
This does not isolate elaboration, kernel checking, or repeated elaboration of
shared Lean definitions within the resident request time.

### Scan-free owned views

The native comparison of main's checked-conversion snapshot (`edb7adfa`) and
the sealed-invariant branch (`f1751222`) used the same sources and Lean binding.
Warm compile numbers are medians of seven alternating pairs. Runtime is
launch-through-exit for eleven alternating pairs of prebuilt binaries. Cold
numbers are one build per workload and revision from an empty proof cache.

| Workload | Cold build, main / branch | Warm build, main / branch | Runtime, main / branch |
|---|---:|---:|---:|
| Append | 3.69 / 7.92 s | 103.2 / 104.7 ms | 26.81 / 25.73 ms |
| String benchmark | 4.01 / 7.18 s | 88.15 / 88.00 ms | 72.32 / 75.57 ms |

Outputs matched exactly. Runtime ranges overlap. Nearly all cold-build change
was in proof checking. A separate repeated-view probe, making 4,096 views of
one 64 KiB string, took a median of 48.13 ms on the main snapshot and 11.63 ms
on the branch, including launch and allocation, and printed 268435456 on both.
The probe isolates repeated calls to `as_str()`; neither application benchmark
does that. The branch's packed ASCII and decimal append still validate bounded
stack bytes. Their performance is outside this measurement.
