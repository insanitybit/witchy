# TOML for Witchy

Reads TOML 1.1 (`Toml.parse`) or 1.0 (`Toml.parse_1_0`). It does not write TOML or keep
comments. The design and what was changed while building it are in `design-toml.md`; times
against the Rust parsers are in `benchmarks.md`.

TOML is a [versioned package](../packages/toml/README.md). Add it as a
Grimoire dependency or name a local checkout in the entry's `.packages`
file. The parser uses the caller's allocator. The example opens a file
using a directory grant supplied by its caller.

```witchy
import console
import fs
import option
import string
import "@toml"

fn main(console: Console<Write>, dir: Dir<Read>, allocator: Allocator):
    let Ok(text): String = dir.read_text(&allocator, "Cargo.toml") else Err(error):
        return
    let Ok(tree): Toml = Toml.parse(&allocator, move text) else Err(error):
        console.print_int(error.line)
        return
    brand tree.doc() as doc:
        let root = doc.root()
        if let Some(item) = doc.get(root, "package"):
            if let Some(package) = doc.table(item):
                if let Some(name) = doc.get(package, "name"):
                    if let Some(found) = doc.str(name):
                        console.line(found)
    tree.close()
```

## What it guarantees

- **No trap.** Every function of the library is a `nopanic fn`: no text, however wrong,
  stops the program. A wrong text is an `Err(TomlError)`.
- **No copy of plain strings.** The document owns the text it was given. A string or a key
  with no escape is returned as a `&str` of that text: no allocation, and no range test,
  because the type states that every node's span lies inside the text
  (`invariant forall i in (0, nodes.len()): nodes[i].start + nodes[i].len <= text.byte_len()`)
  and Lean shows it where nodes are written. A string with an escape is decoded once, during
  the parse, into one side text the document owns.
- **A handle belongs to its document.** `Table<G>`, `Array<G>` and `Item<G>` carry the brand
  of the opening they came from. Using one on another document is refused when the program
  is compiled (`tests/fixtures/toml/other-document.refused.wy`).
- **A fixed number of allocations.** The work array, the key index, the node array and the
  decoded text are each sized once from a count of the text's `=`, `[`, `,` and `.`: the
  same number of allocations a parse at 100 KB and at 8 MB.

How the second is shown: `scripts/toml-traps.py` asks the compiler's trap report
(`witchyc wasm-traps`) about a program that reads strings, and no function of the library
is on it. In the native code of `Toml.value_text` (build with `WITCHY_NATIVE_NAMES=1`, then
`otool -tv -p _witchy_function_N_Toml_value_text`) the node's `start` and `len` are loaded,
the address is `text + start`, and nothing is compared with the text's length. Two tests
remain and neither is a range test: the bytes at the two ends are tested for being inside a
character, which is what makes the result a `&str`, and on native the sum `start + len`
keeps an overflow test that cannot fire (two 32-bit numbers; see `benchmarks.md`).

## Reading

| Call | Gives |
|---|---|
| `Toml.parse(&allocator, text)`, `Toml.parse_1_0` | `Result<Toml, TomlError>`; the text is moved in |
| `tree.count()` | how many values, tables and arrays among them |
| `tree.close()` | releases the document and the text |
| `brand tree.doc() as doc:` | opens it for reading; handles live inside the block |
| `doc.root()` | the top table |
| `doc.get(table, "key")` | `Option<Item<G>>`, one probe of the document's key index |
| `doc.kind(item)` | a `TomlKind` |
| `doc.key(item)` | the key as `&str` (empty for a value of an array) |
| `doc.str(item)`, `doc.i64`, `doc.f64`, `doc.bool`, `doc.datetime` | `Some` when the value is of that kind |
| `doc.table(item)`, `doc.array(item)` | the handle for a container |
| `doc.table_len(table)`, `doc.array_len(array)` | how many values it holds |
| `doc.first(table)`, `doc.next(item)` | a table's values in the order written |
| `doc.first_element(array)`, `doc.next(item)` | an array's values in order |

A `TomlDateTime` has `kind` (6 offset date-time, 7 local date-time, 8 local date, 9 local
time), the fields that kind has, `nanosecond` (digits past the ninth are dropped), and for an
offset `offset_minutes` with `west` true for a negative one. Numbers are decoded during the
parse; a float is rounded correctly (a short decimal exactly, a long one by exact decimal
arithmetic).

## Errors and limits

`TomlError` has `at` (a byte offset), `line` and `column` (from 1) and `what`, a
`TomlProblem`: `Ended`, `Unexpected`, `Control`, `Escape`, `Number`, `Date`, `Duplicate`,
`Redefined`, `NotTable`, `Closed`, `Deep`, `Large`, `Memory`, `Internal`. No message is
allocated.

- A text of 4 GiB or more is `Large`: offsets are 32 bits and are never wrapped.
- More than 256 arrays and inline tables open at once is `Deep`. The reader does not recurse,
  so nesting cannot exhaust the stack.
- A float whose digits are too large for any finite number (`1e500`) is refused, as an
  integer outside 64 bits is. Only `inf` is infinity. A float whose written
  power is a million or more is refused too (review: its seventh digit was once dropped).
- An allocation the allocator refuses is `Memory`.
- A key is looked for in at most 256 cells of the index. Keys fill at most half of it, so only
  keys chosen to share cells reach that; such a document is refused (`Memory`) instead of being
  read in time that grows with the square of its keys (review). `scripts/toml-flood.py` checks it.

## Tests

- `scripts/toml-test.py`: the toml-test corpus (`tests/toml/toml-test`, v2.2.0), for 1.0 and
  1.1, through `tests/toml/dump.wy` on the Wasm host or a native build. A valid case must
  match its tagged JSON; an invalid one must be refused. No message text is compared.
- `scripts/toml-fuzz.py`: mutants of the corpus, read by `tests/toml/accepts.wy` and by the
  Rust `toml` crate (`benchmarks/toml/rust`, `oracle`). Fails on a text Witchy reads and the
  crate refuses.
- `tests/fixtures/toml/state-*.wy`: one fixture for each way a table was made (on the way to
  a header, by a header, by a dotted key, inline, as an array of tables) against a header
  and against a dotted key naming it again.
- `scripts/toml-differential.py`: generated TOML 1.0 documents (tables, dotted keys, arrays of
  tables and inline tables over a few keys, written plainly, quoted and escaped; every form of
  number, date and string), read by Witchy and by Python's `tomllib`: read by both or refused by
  both, and every key and value the same, in the order written.
- `scripts/toml-allocations.py`: the allocation count at three sizes.
- `scripts/toml-traps.py`: the trap report names no function of the library.
