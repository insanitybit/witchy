# Console text

`Console<Write>` has two operations:

- `console.print_int(n)` writes a `U64` in decimal and a newline.
- `console.print_byte(b)` writes one `U8` as it is.

Text is written by library code, a byte at a time (`library/wasm-console.wy`):

```
import "library/wasm-console.wy"

fn main(console: Console<Write>, allocator: Allocator):
    console.line("Hello, Witchy.")      // a literal and a newline
    console.write("count: ")            // a literal
    console.print_int(3)
    console.write_string(&text)         // a String
```

## Why a byte at a time

The host import `print_u8(capability, value)` takes the byte itself. The host
therefore still reads no guest memory, and the argument for store confinement
(`docs/wasm-store-confinement.md`) is unchanged: nothing outside the module
looks at either memory. A `(pointer, length)` import would be faster per call
and would add host reads of guest memory to the trusted base.

## What changed in the runtime

- A module that uses the console imports `print_u64` then `print_u8`, both of
  type `(i32, i64) -> ()`; a module that does not imports neither. The launcher
  (`tools/wasmtime-host/src/profile.rs`) accepts exactly those lists.
- The engine (`tools/wasmtime-host`) refuses a byte outside 0..255 and counts
  bytes against the same output budget, 65,536 bytes unless `--output-bytes=N` grants more
  (`benchmarks/fasta.wy` at n = 250000 writes 2.5 MB). Output is still committed
  only when the run succeeds.
- Hosts built before this change refuse modules that print, since the second
  import is unknown to them.

## Trusted

The engine's `print_u8` body (four lines), with the rest of the host. The
checker types the argument as `U8`; the engine's range check does not rely on
that.

## Evidence

`tests/fixtures/console/hello.wy` prints two literals and a built String
through Wasmtime. `tests/fixtures/console/wide.refused.wy`: a `U64` is not
accepted as a byte.

## Input

```
import "library/wasm-console.wy"

fn main(console: Console<Write>, input: Console<Read>, allocator: Allocator):
    var text = String.new(&allocator)?
    loop:
        match input.read_line(&allocator, &mut text):
            Line: ...                    // `text` holds the line, without its newline
            End: break                   // nothing more; a last line without a newline is a Line
            NotText: ...                 // the line was not UTF-8; it has been skipped
            NoRoom: ...                  // the heap is full
```

`Console<Read>` has two operations. `input.next_byte()` gives the next byte as a `U64`, or 256
once the input has ended. `input.next_run()` (2026-11-19) gives up to seven bytes in one call,
lowest first, with their count in the top byte (0 once the input has ended); a run ends after a
newline or after a byte of 128 or more, so nothing in it belongs to the next line and a scalar's
continuation bytes are read with `next_byte`. `read_byte`, and `read_line` with its UTF-8 check,
are library code built on them (`read_line` on runs: 200,000 short lines 19.2 -> 17.3 ms). `String.clear` and
`Vec.truncate_mut` were added for reuse of one line buffer.

`main` may take `Console<Write>`, `Console<Read>`, `Allocator` and the other
capabilities the host can grant (`Clock`, `Rand`, `Dir`, `File`, `Net`, ...: docs/wasm-capabilities.md),
each at most once, in any order. Like the other two, the input grant cannot be made, copied
or returned by a program; it is passed on by reference.

### Binary input

Use `import console_bytes` to read into a caller-owned mutable byte slice.
`input.read_exact(destination)` fills that slice and leaves any following
byte available to the next read. At EOF before the slice is full,
`ShortConsoleInput(count)` reports the prefix already written. The rest
of the slice retains its original bytes.

Use `input.read_u24_be()` for an unsigned three-byte big-endian field.
It returns a `U64`, uses a three-byte local buffer, and leaves the
following byte unread. If the stream ends early, `ShortConsoleInput(count)`
reports how many bytes were consumed. Check any protocol length limit
before allocating or reading the body.

Use `input.read_exact_to_end(destination)` for a final body whose expected
length comes from an earlier message field. It errors with
`TrailingConsoleInput` if one extra byte follows the body, consuming that
byte while checking for EOF. Both methods accept binary data and allocate
no storage. The host's input byte budget and deadline still apply. Coven
uses these methods for bounded release, trust, and CI worker bodies.

Use `input.read_line_bytes(destination)` for an LF-framed binary or
text header before a body. It fills up to the provided slice length and
returns the number of bytes preceding LF. The LF is consumed and the
next input byte remains available. An empty LF frame fits an empty slice.
`MissingLineFeed` means EOF arrived before LF. `LineTooLong` consumes only
the first excess byte and leaves the rest of the input unread. These
refusals leave the already filled prefix in the caller's slice.

Use `input.read_optional_line_bytes(destination)` for a sequence of LF
frames that can end at a frame boundary. It returns `Ok(EndOfFrames)` if EOF
arrives before any byte of the next frame, `Ok(LineFrame(0))` for an empty LF
frame, and `Err(MissingLineFeed)` for a nonempty frame cut short by EOF.
It uses the same caller-owned storage and overflow behavior.

`input.read_text_frame(allocator, limit)` applies the same framing to a
UTF-8 header and returns an owned `String`. It reserves at most the
caller-chosen limit for the frame buffer before reading. A valid frame's
string takes ownership of that buffer. It reports malformed UTF-8 separately
from a missing delimiter, overflow, or an allocation failure. The TUF
download and CI request workers use this method before JSON parsing.

### Runtime

- The launcher and engine take `--input=FILE` (the engine wants an absolute
  path) and `--input-bytes=N` (default 65,536, at most 16 MiB). The engine reads
  the whole file before the program starts and refuses a file larger than the
  grant.
- `--input-id=DEVICE:INODE` (2026-11-22), from a launcher that examined the file: the engine
  opens the path without following a link and refuses anything but that regular file.
- `--input-stdin` (2026-11-22) makes the engine's own standard input the one input instead:
  a stream, handed to the program 64 KiB at a time as it reads, so a tool in a pipe answers
  before its input ends and reads more than it has memory for. `--input-bytes` is then a
  running budget (any number, or `unlimited`): the read that would pass it ends the run with
  an error, after the program has already run on what came before. Never the object a secret
  is read from, and never a stream while the program takes a secret: it is then read whole
  before the program starts, within 16 MiB, as before. The deadline holds while the program
  waits for input. One of the two, not both. In a grant
  document: `[input]` with `input = "requests.txt"` (a file, from the document's directory) or
  `input = { stdin = true }`; under Grimoire each is a `grant: input:` line and
  `[run] input-bytes` its limit. Without the flag nothing is read from a terminal or a pipe.
- A module whose emitted functions hold the grant imports `read_u8` and `read_run`, both of type
  `(i64) -> (i64)`, after the print imports (a module built before `read_run` imports `read_u8`
  alone, and still runs). A module that only imports the library
  does not. The engine refuses a module that imports `read_u8` without
  `--input`, and one run with `--input` that does not import it.
- A fixture names its input in `FIXTURE.flags` (`run: --input={dir}/name.txt`).

### Trusted

The engine's `read_u8` and `read_run` bodies and the code that reads the file, with the rest of
the host. As with printing, the host hands the guest a value and reads or
writes no guest memory, so the store-confinement argument is unchanged. The
caller of the engine chooses the file; a program cannot name one.

### Evidence

`tests/fixtures/programs/sums.wy` reads lines of numbers and prints their sums;
`console/echo.wy` copies its input; `console/not-text.wy` meets a line that is
not UTF-8. Refused, each with a fixture in `tests/fixtures/console/`: two input
grants in `main`, `Console<Other>`, `next_byte` on `Console<Write>`, printing to
`Console<Read>`, and a program that tries to construct the grant.

## Not done

Input is one file or the launch's standard input, read whole before the program starts: no
interactive input, and no command-line arguments (design: docs/decisions.md, 2026-11-22; files a program names go through
`Dir` and `File`: docs/wasm-files.md). `write_u64` writes a number without a newline.

## Interpolated text: a value

```
let message = "total ${total}, ready ${ready}"
console.print(message)                        // total 42, ready true; nothing allocated
text.push_format(&allocator, message)?        // appends the same text to a String
```

An interpolated literal is a value of its own, a *format* (docs/design-format.md): its text
pieces and its values, copied in when it is made. It builds no text. `console.print` and
`String.push_format` are ordinary library functions over the `Format` trait; each value is
written by its `Show`, taken by value (`U64`, `Bool`, text, `Span`, and a program's own
copied type that implements `Show`); a String is shown through its view, `${name.span()}`
(`console/print-span.wy`). A plain literal is a `Format` too. Fixtures: `console/print.wy`,
`console/push-format.wy`; refused: an unclosed `${`, two expressions in one, a type without
`Show`, and in this first version an owned `String` as a piece (`print-string.refused.wy`), and closing a String while a format
holding its view is in use (`print-span-closed.refused.wy`).
