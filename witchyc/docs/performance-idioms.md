# Writing fast Witchy: measured idioms

Each line below was measured against the alternative it names, with the same output, on the
wasmtime host (docs/decisions.md and docs/benchmarks.md, 2026-11-18/19, have the runs). They are
about the library as it is; a later library change can move them.

## Vectors and strings

- **Grow in place.** `v.push_mut(&allocator, x)`, `s.push(&allocator, c)`, `s.push_text(...)`,
  `s.push_string(...)` change the value where it is. The by-value forms (`v = v.push(...)`,
  `s.push_scalar(...)`, `s.append(...)`) go through the proved path and its reserve on every call:
  a JSON tree built with `push` parsed 625 ms against 543 with `push_mut`; `show_scalar` moved to
  `push` cut JSON writing 377 -> 297 ms.
- **Test an append with `?`, `if let` or `match`, not `.is_ok()`.** The appends answer
  `Result<(), U64>` (2026-11-22). `text.push(a, c)?` and `if let Err(_) = text.push(a, c):` cost
  3 instructions more than the `Bool` they replaced (of about 290 a push); `.is_err()` looks at
  the result through a reference and costs 27 more. `benchmarks/witchy/words.wy` written with
  `.is_err()` ran 3.9% more instructions, with `if let` 0.6%.
- **Copy bytes, not scalars.** Between two scalar boundaries of a String, `push_part(source, from,
  to)` copies bytes eight at a time; decoding each scalar and pushing it again was the cost in
  `push_slice` (CSV split -18%) and both JSON directions (-28% parse, -26% write).
- **Move bytes in runs.** `target.copy_from(source)` between slices, `v.extend_from_slice_mut(&allocator,
  bytes)`, `String.from_utf8_bytes(&allocator, bytes)`, `text.push_utf8_slice(&allocator, bytes)`,
  `span.to_string(&allocator)`, `text.push_string`, `text.push_repeated` and `Vec.filled` each go a
  word at a time with one allocator call for the run: 4 to 9 instructions a byte for the copies and
  fills, about 20 with the UTF-8 test (2026-11-22). A loop of `push_mut` or `push` costs 210 to 290 a
  byte, and `for i in 0..n: a[i] = b[i]` through slices about 50.
- **Size once.** `Vec.new(&allocator, n)` and `HashMap.for_entries(&allocator, n)` when the count
  is known; a map grown from 16 cells spends about a fifth of a counting run moving entries.

## Reading text

- **Walk with `s.chars()`**, not `for c in &s`: the generator reads ASCII straight from the byte
  slice; `for c in &s` calls `next` with a cursor kept in memory (24.1 against 34.8 ms over 6M
  scalars).
- **Compare over slices.** `a == b` on Strings, `compare`, `starts_with_text`, `equals_text` and
  `Span.equals_text` already read byte slices, and Text literals a word at a time; a loop of
  `byte_at` written by hand is slower than these, though one `byte_at` alone is cheap.
- **Index a `&[U8]` from `s.as_bytes()`** in a loop to its own `len()`: the reads carry no test
  (docs/wasm-iteration.md).

## Maps

- **Look literals up as `map.get(&"key")`.** A Text key hashes and compares a word at a time,
  reducing its hash twice per word (proved) and comparing a String a word at a time
  (`String.word_at`): long literal lookups 52% faster than before, 2026-11-19.
- **Counting:** `get_mut` then `put` on a miss searches twice. There is no single-probe
  insert-or-update yet (docs/benchmarks.md, `map`).
- **String keys** hash six bytes per remainder with no overflow check (proved), and compare as
  slices: long keys are now cheap to look up (127-byte keys 205 -> 111 ms).

## Output

- **Write text, not bytes.** `console.write`, `write_string` and `write_u64` send eight bytes per
  host call (`print_word`, 2026-11-19: 3.5x on a write-heavy run). A program that makes its output a
  byte at a time puts them into a `WordWriter` (`writer.put(&console, b)`, then `flush`), as Go
  programs use a buffered writer: fasta 35.5 -> 32.0 ms.

- `console.write("literal")` and `console.write_string(&s)` are fast; a host call per byte costs
  about 3.6 ns, so batching output yourself gains little.

## Proofs as an optimization

A function with `proof: auto` has its arithmetic checks removed where the prover shows them
unnecessary: `hash_bytes` reduces once per six bytes because its loop invariant `hash < p` makes
six steps fit in U64, and no overflow test is emitted. A function without a contract keeps every
check, however obviously bounded: `(*seed % 139968 * 3877 + 29573) % 139968` still tests its
multiply, so `fasta`'s generator is not made faster by rewriting it.
