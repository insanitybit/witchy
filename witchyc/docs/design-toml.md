# Design: a zero-copy TOML 1.1 reader

Status: implemented (2026-11-25) and now shipped as `packages/toml/toml.wy`, with the changes listed under
"As built" at the end. Use: `wasm-toml.md`. Times: `benchmarks.md`. The speed goal was
missed: a parse takes 3.4 times toml-spanner's time and half of `toml`'s.

## Goal

Read TOML 1.1 faster than the fastest Rust parser, toml-spanner, without a trap on any input.
toml-spanner reports 24.5 µs on zed's Cargo.toml, against ~228 µs for `toml` and ~307 µs for
`toml_edit`. Those are the author's own numbers, so we rerun them in `benchmarks/rust/`.

Its speed comes from layout, not SIMD:

- strings borrowed from the source;
- one bump arena for everything it builds;
- 24-byte values.

Witchy can do each of those. It can also give two guarantees toml-spanner does not have:

- **No trap:** `nopanic`, instead of `unsafe` checked only by fuzzing and Miri.
- **No mixed handles:** a handle from one document cannot be used on another, enforced by
  brands. toml-spanner relies on lifetimes for this.

Writing TOML back out and editing it while keeping comments and layout are left out, as in
Python's `tomllib`.

## Shape

```
let text: &str = file.as_str()                 // UTF-8 already proved: no validation pass
brand toml.read(&allocator, text) as doc:      // Result<Doc<G>, TomlError>
    let root = doc.root()                      // Table<G>
    let name = doc.get(root, "package")?.table()?.get_str(doc, "name")?   // &str into `text`
    let jobs = doc.get(root, "jobs")?.i64()?                               // I64 leaves the brand
    doc.close()
```

- `Doc<G>` is a sealed type. It holds the node arena, the key index and the source's
  `&str`, so the source stays lent and cannot change while the document is open.
- `Table<G>`, `Array<G>` and `Item<G>` are sealed `U32` node numbers from one opening, the
  same pattern as `Node<G>` in `wasm-graph.wy`.
- Scalars (`I64`, `F64`, `Bool`, dates packed in a `U64`) carry no brand, so they can leave
  the opening.
- A `&str` result borrows the source, not the document.

## Input: take `&str`, not bytes

TOML input must be valid UTF-8. A `&str` already carries that guarantee: `String.as_str()`
is free, and `str.from_utf8_bytes` runs the shared validator once. The reader therefore does
no UTF-8 pass. It checks only TOML's own byte rules: no control characters other than tab,
and newlines only where allowed.

## Stage 1: skip kernels (SIMD)

No paper covers SIMD TOML. simdjson's first pass builds a map of every structural byte. That
pays off for big JSON, but not for TOML, which is mostly small line-oriented config files.

Use simd-csv's approach instead: kernels that skip runs of 16-byte blocks until something
interesting appears for the current state. Each one is a few `U8x16.cmp_eq`, `cmp_lt` or
`cmp_gt` calls OR'd together, then `bitmask()`:

| State | Stops at |
|---|---|
| basic string | `"` `\` byte < 0x20 except tab, 0x7F |
| literal string | `'` controls |
| multi-line string | as above, plus `\n` (to count lines for errors) |
| comment | `\n` controls |
| whitespace | anything but space/tab |
| bare key | anything outside `A-Za-z0-9_-` (two range compares, one OR) |

While scanning a string body, the kernel ORs a mask of `\` hits into one flag. A string with
no escapes is borrowed. One with escapes is marked and decoded when read.

**Gaps to close first:**

- **Bit scan (compiler).** There is no `U64.trailing_zeros()` or `count_ones()`.
  `find_byte_from` today stops at a matching block and then rescans it a byte at a time. Add
  them as compiler built-ins: Cranelift `ctz`/`popcnt`, Wasm `i64.ctz`/`i64.popcnt`. They
  cannot trap, so `nopanic` holds.
- **SIMD on the Wasm host.** `wasm-span.wy` uses SIMD only under
  `static if target.backend == .Native`, because `wasm-frame-host.md` does not admit SIMD128.
  The scalar path stays as the fallback on that host.
- **Byte classes without a shuffle.** There is no byte-shuffle operation (`pshufb` /
  `i8x16.swizzle`). Every TOML byte class above takes four compares or fewer, so none is
  needed.

## Stage 2: a flat arena of nodes

A Vec element may not hold a view (`design-references-to-views.md` rule 3), so a node cannot
hold a `Span`. Nodes therefore hold offsets into the source:

```
type Node:           // 24 bytes, scalars only
    kind: U8         // table, inline table, array, array of tables, string, int, float, bool, 4 date kinds
    flags: U8        // escaped, multi-line, in decoded buffer, table state (below)
    start: U32       // byte offset of the text (strings) or the key
    len: U32
    next: U32        // next sibling, in document order
    data: U64        // I64/F64 bits, packed date, or first child + count for containers
```

**Allocation.**

- Before parsing, a SIMD pass counts `=`, `[` and `,` with `count_ones`. The parser then
  sizes the node Vec once with `Vec.new(&allocator, n)`.
- Escaped strings decode into one side `String` owned by the document, flagged
  "in decoded buffer".
- A parse makes about 3 allocations whatever the input size. toml-spanner's bump arena gets
  this; here it comes from presizing alone.
- Source offsets are `U32`. The reader refuses input over 4 GiB rather than wrapping: it
  fails closed.

**Scalars are decoded during the parse.** The parse must validate them anyway, so the value
lands in `data` and a read is a single load. Integer overflow is already a TOML error, so the
accumulator uses checked steps that return `Err` and never trap.

**The parser is iterative.** Nested arrays and inline tables keep an explicit stack in a Vec
instead of recursing, so hostile nesting cannot exhaust the stack. A depth cap is a
`TomlError`.

## Key index: one table for the whole document

TOML has to look keys up while parsing, to reject duplicates and to reopen tables named
again through dotted keys or `[a.b]` headers.

- One open-addressing table of `U64` cells per document, mapping
  `(parent node, foldhash(key bytes))` to a node number. There are no owned `String` keys and
  no map per table.
- An escaped key hashes and compares its decoded bytes. Such keys are rare.
- After the parse, the same table serves `doc.get(table, "key")` in one probe.
- Iterating follows `next` links, so it keeps document order at no extra cost.
- Cells hold only numbers, so the table needs no `Discard` work when the document closes.

Each table records how it was created. That state decides which redefinitions are errors:

| State | Created by | `[header]` again | Dotted key into it |
|---|---|---|---|
| Implicit | an intermediate path of a header | allowed, becomes Explicit | allowed, becomes Dotted |
| Explicit | `[a]` | error | allowed within the same header |
| Dotted | `a.b = 1` | error | allowed only from the same table |
| Frozen | inline `{...}` | error | error |
| ArrayOfTables | `[[a]]` | `[[a]]` appends | into the last element |

## `nopanic` and proofs

Every function in `packages/toml/toml.wy` is a `nopanic fn`. Untrusted TOML can then never stop the
program, and the checker proves this; it is not just tested.

- **Cursor helpers** have scalar contracts (`requires at <= bytes.len()`,
  `ensures result <= bytes.len()`). The prover drops their overflow and bounds checks, as it
  already does for `hash_bytes` and counted slice loops.
- **Reads by the parser** use guarded forms (`at < len && bytes[at] == c`,
  `byte_or_end`), which count as unable to trap.
- **Errors** are `TomlError { at: U32, what: TomlProblem }`, a sum type with no allocation on
  the error path. A message is built only when printed, with line and column computed from
  `at`.

**`Doc` invariants.** The sealed `Doc<G>` keeps
`invariant source_len < 4294967296` and `invariant nodes.len() < 4294967296`. Invariants may
read a Vec's length since 189908c6.

**One proof gap.** An invariant cannot yet constrain each element of a Vec ("element values
unread", the same limit hit on `pq`). So `start + len <= source_len` cannot be stated for
every node.

- **Today:** a string read through a node keeps one runtime comparison. It returns `Err`, not
  a trap, so `nopanic` still holds.
- **With element invariants** (`invariant forall i < nodes.len(): ...`, the next step after
  proved containers): the comparison goes away. The parser is the only code that writes
  nodes, so the obligation lands on it.

## Brands: what they buy

- `Item<G>` is a bare `U32`. Without the brand, passing a handle from one document to
  another would read a wrong node that is still in range. With it, that is a type error at
  compile time, at zero runtime cost: brands are erased before code generation, so there is
  no generation word as a slot arena would need.
- The open document lends the source, so a returned `&str` stays valid without copying.
- A future proof of "handle from this opening, so below `nodes.len()`" would drop the last
  index check. `design-brands.md` already lists this as a proposed extension.

## Typed reading

toml-spanner loses most of its lead once values are converted into user types. Here a
`derive(TomlRead)` recipe, in the same family as the existing derives, generates a reader
that walks a record's fields straight from handles, one key-index probe per field, with no
intermediate tree. It is the last step.

## Tests and measurement

- **toml-test corpus** (valid/invalid, 1.0 and 1.1) as fixtures. Assert the class (accepted,
  or refused with the right `TomlProblem`), never message text. Valid cases compare against
  toml-test's tagged JSON, written with `wasm-json-tree.wy`.
- **Fast direct tests** for the table-state matrix above, one per cell.
- **Fuzz** with `scripts/fuzz-mutants.py`. `nopanic` makes "never traps" a compile-time
  fact, so fuzzing targets wrong answers, not crashes.
- **Benchmarks** in `benchmarks/rust/` against toml-spanner 1.0, `toml` and `toml_edit` on
  toml-spanner's own inputs (zed Cargo.toml and others), plus a 1 MB generated file to
  exercise the SIMD kernels. Run on release builds with alternating pairs, as
  `docs/benchmarks.md` does.

## Order

1. `U64.trailing_zeros` / `count_ones` built-ins, plus the six skip kernels as `nopanic`
   functions, each with direct tests.
2. The arena, key index and table-state machine; the toml-test corpus passing.
3. The branded `Doc<G>` API, examples and docs.
4. Benchmarks against the Rust crates; find hot spots with `wasm-traps` and the native
   profile.
5. Element invariants on `Doc` (shared with the proved-containers work), then
   `derive(TomlRead)`, then a plain writer.

## As built

What the library does differently from the text above, and why.

- **The document owns the text.** `Toml.parse(&allocator, text: String)` takes the `String`
  and `tree.close()` releases it. A type that holds a reference can state no invariant and
  no contract, and the element invariant that removes the range test needs both. Views
  returned by `doc.str` borrow the document.
- **Two steps: `Toml` then `Doc<G>`.** The parse gives a `Toml`; `brand tree.doc() as doc:`
  opens it, and handles carry that opening's brand. A document outlives its openings and
  may be opened again.
- **Read into slices, then sealed.** On native every cell of a `Vec` is reached through the
  runtime unless the function is proved. The parse therefore works in two plain `U64`
  slices (nodes of four words, and the key index), with free functions, and `Toml.seal`
  pushes each finished node once through the one proved function that keeps the invariant.
  A function that writes the fields of a sealed value may not call its methods, which is
  the other reason the parser is not written as methods on the document.
- **Nodes are four words while parsing and a 32-byte record in the document**, with a key
  span and a text span each (`key_start`, `key_len`, `start`, `len`), so there are two
  element invariants, one for each span. A type may now state several over one field.
- **No vector kernels.** `U8x16` operations are calls on native and slower than a loop. The
  string scanner and the UTF-8 check read eight bytes at a time (`read_le64`) and find the
  byte with `U64.trailing_zeros`. Bare keys, space and comments go a byte at a time.
- **Dates are checked during the parse and decoded when read**, from the node's span: a
  node has one data word, and a date with nanoseconds and an offset does not fit in it.
- **Floats** use the exact short path when the digits and the exponent allow, and exact
  decimal arithmetic otherwise, so every float is correctly rounded.
- **Children are a circular list** through `next`, and a container's data word holds its
  last child and its count: adding is one write, and reading in order starts at `last.next`.
- **Errors** carry `line` and `column`, computed only when a parse fails.
- **Not built:** `derive(TomlRead)` and a writer.

Compiler work this needed, beyond element invariants and the bit scans: `part_proved` (a
part of a slice or a `&str` with no range test where the proof shows the range),
`read_le64`, a shift by a literal below the width counted as unable to trap, a literal given
for a plain parameter of a generic method, one runtime fill for `Vec.filled`, and
`WITCHY_NATIVE_NAMES` and `WITCHY_COUNT_ALLOCATIONS` for measuring.
