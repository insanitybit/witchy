# An introduction to capabilities

Imagine lending someone the key to a filing cabinet so they can look up a form. The task doesn't require giving them keys to the entire building. In Witchy, a **capability** is a value that gives a piece of code authority to perform an operation.

The person or trusted launcher running the program decides what authority to grant. The program's entry point receives it as parameters. The program then chooses which values to pass into its libraries. A library can also receive authority indirectly, for example through a record or a closure that captures a capability.

## Read one directory

This program reads `in.txt` from a directory supplied at launch and prints the byte length. Save it as `count-bytes.wy`.

```witchy
import console
import fs
import string

fn main(console: Console<Write>, dir: Dir<Read>, allocator: Allocator):
    let Ok(input): String = dir.read_text(&allocator, "in.txt") else Err(code):
        console.line("Could not read in.txt")
        dir.close()
        return
    console.print_int(input.byte_len())
    input.close()
    dir.close()
```

`Dir<Read>` is a directory handle with read rights. The path is relative to that handle. The allocator gives `read_text` memory for its result. A successful read produces an owned string, which the program closes after printing. Both paths close the directory.

```sh
mkdir -p sample
printf 'abc' > sample/in.txt
witchyc build count-bytes.wy -o count-bytes.wasm
witchy-wasmtime-host count-bytes.wasm --output --allocator --dir="$PWD/sample" --dir-rights=read
```

The result is `3`. A launcher can pick a different directory without changing the binary. The program gets access through the supplied directory handle. Passing that handle to another function delegates read authority to that function. Passing just the bytes lets it parse the input without getting the ability to open more files.

## Grant, narrow, delegate

| Step | Who decides? | Example |
|---|---|---|
| Grant | The trusted launcher | Supply a read-only directory rooted at the chosen path |
| Receive | The entry point's declared interface | Accept `Dir<Read>` and `Allocator` |
| Delegate | The caller | Pass a borrowed directory to a loader, or pass only its text to a parser |

Try changing the read to a write. The checker rejects writes through a read-only directory capability. See the [write refusal fixture](../tests/fixtures/diagnostics/dir-read-write.refused.wy). Libraries can also define sealed wrappers over existing grants and expose only the operations their callers need.

These examples use the Wasmtime host. Native launch has a smaller supported grant set and runs with the OS process's authority. Read the [target coverage](../docs/native-backend.md#supported-operations-and-trust-boundary) before relying on a host grant.

[Grant and wrapper reference](../docs/wasm-capabilities.md) · [Files and directory rights](../docs/wasm-files.md) · [Network and HTTP](../docs/wasm-http.md) · [Package tests](../docs/language.md#package-tests)
