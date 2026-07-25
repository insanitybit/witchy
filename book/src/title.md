# The witchy Book

*A guide to a capability-secure language where a program's authority is a typed,
auditable, enforceable artifact.*

witchy is a small, statically typed language in which a program's authority is visible
in its types and enforced at runtime by a sandboxed WebAssembly VM. There is no
ambient authority: a function without a filesystem capability cannot access the
filesystem, and the compiler rejects the attempt.

This book teaches witchy from the ground up. For an exhaustive description of
the syntax, see the
[language reference](https://github.com/insanitybit/witchy/blob/master/spec/language.md);
this book is the narrative path.

Every `witchy` block in this book is a complete program that the test suite
type-checks. Blocks classified as runnable are also executed against the
committed output oracle; blocks requiring unavailable host authority are
read-only.

> **Try it as you read.** The compiler itself runs in WebAssembly, so the
> [browser playground](https://github.com/insanitybit/witchy/tree/master/web) can
> compile and run the book's runnable examples without an installation.
