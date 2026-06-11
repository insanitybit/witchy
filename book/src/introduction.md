# Introduction

## The problem witchy is about

When you run a program — or `npm install` a dependency, or a CI job — it
inherits *your* authority. A library that formats dates can also read your SSH
keys, open a socket, and delete files, because nothing in the language stops
it. We call this **ambient authority**: the power to act on the world is just
*there*, available to any code that runs. Most security incidents in software
supply chains are ambient authority being used by code you didn't write and
didn't read.

witchy removes it. Authority is not ambient — it is a **value**, with a type,
that must be passed explicitly from caller to callee. A function's full power is
its parameter list:

```witchy
// This function can read a file. You can see that. It cannot write, connect to
// the network, or read the clock — there is no parameter that would let it.
fn first_line(dir: Dir[Read], name: String) -> String:
    let contents = read(dir, name)
    contents

fn main(console: Console, dir: Dir[Read]):
    print(console, first_line(dir, "notes.txt"))
```

A function with no capability parameters — like a pure helper that adds two
numbers — *provably* has no effects. Not by convention; by construction.

## Three things this makes possible

**You can audit by reading signatures.** `witchy caps program.witchy` walks the
program and reports its complete capability footprint, computed from the source,
broken down per right (`Dir[Read]` vs `Dir[Write]`, `Net[Connect]` vs
`Net[Listen]`). It is never self-asserted metadata that could drift or lie.

**You can gate on growth.** `witchy caps-diff old new` fails when authority
widened. Put it in CI and a dependency cannot quietly start listening on a
socket between versions. The package manager applies the same gate to the runes
(packages) you depend on.

**You can enforce at runtime.** `witchy sandbox program.witchy` compiles to
WebAssembly and runs it in a VM that has been handed *exactly* the host
functions its footprint calls for — and nothing else physically exists for it
to call.

## One language, one meaning, two ways to run it

witchy has two backends:

| Backend | What it's for |
|---|---|
| **Interpreter** | The reference. Fast to start, used during development. |
| **WebAssembly** (via wasmtime) | Deployment — confinement *and* speed: the capability boundary becomes the VM boundary, and the tier benches at native class. Also the browser playground. |

These are not two dialects. They are held to a single invariant the project
calls **parity**: a program produces *identical* output on both, down to
error behavior, and the test suite enforces it. When a backend cannot do
something the same way, that is a loud compile-time error — never a quietly
different answer. You will see this idea return throughout the book, because it
is what lets you trust that the sandbox runs the same program you tested.

## Who this book is for

You should be comfortable programming in *some* language. We don't assume Rust,
though witchy borrows Rust's vocabulary (`fn`, `match`, `trait`, `impl`) and
Python's layout (indentation, not braces). We *do* assume you're curious about
what it would feel like if "what can this code do?" had a precise, mechanical
answer.

Let's get it running.
