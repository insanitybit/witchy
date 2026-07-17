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
[language reference](https://github.com/insanitybit/witchy/blob/master/spec/language.md)
is its companion; this book is the narrative path.

Every `witchy` block in this book is a complete program that the test suite
type-checks. Blocks classified as runnable are also executed against the
committed output oracle; blocks requiring unavailable host authority are
read-only.

> **Try it as you read.** The compiler itself runs in WebAssembly, so the
> [browser playground](https://github.com/insanitybit/witchy/tree/master/web) can
> compile and run the book's runnable examples without an installation.
