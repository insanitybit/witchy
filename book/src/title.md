# The witchy Book

*A guide to a capability-secure language where a program's authority is a typed,
auditable, enforceable artifact.*

witchy is a small, statically-typed language with an unusual promise: you can
tell exactly what a program — or any function within it — is allowed to do just
by reading its types, and you can **enforce** that at runtime by compiling to a
sandboxed WebAssembly VM. There is no ambient authority: a function that doesn't
receive the means to touch the filesystem cannot touch the filesystem, and the
compiler will not let it try.

This book teaches witchy from the ground up, building toward that idea. If you
want the terse, exhaustive description of every form, the
[language reference](https://github.com/insanitybit/witchy/blob/master/docs/language.md)
is its companion; this book is the narrative path.

Every witchy example in this book is a complete program that the project's test
suite type-checks and runs, so what you read here is what the language actually
does today.

> **Try it as you read.** The interpreter compiles to WebAssembly, so there's a
> [browser playground](https://github.com/insanitybit/witchy/tree/master/web) —
> paste any `Console`-only example in and run it with no installation.
