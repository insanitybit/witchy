# Security Policy

If you think you've found a security issue in witchy - a sandbox escape, a
capability bypass, anything in that family - open a GitHub issue.

I'd rather reports start out private (GitHub's "Report a vulnerability" flow
works) so a fix can land before the details spread. But it's your finding, and
how you disclose it is your call.

## How capabilities are represented, and where that stops

The compiled backend hands live host authority to the guest as an opaque
WebAssembly `externref`. A capability reference never enters linear memory, so
you can't forge one by corrupting an integer - there's no integer to corrupt.

Typed Wasm GC structs and arrays carry those references through the shapes you'd
expect to nest them in: concrete tuples and nominal instances, closure
environments, concrete function signatures, `Option`/`Result`, and `List` values
that hold references.

Four boundaries have no concrete reference-aware layout, and the compiler
rejects them at check time rather than guessing:

- a `Dict` whose keys or values carry references,
- an open generic function ABI instantiated with a capability-bearing value,
- a `region:` copy-out carrying a capability,
- a capability-typed callback crossing an isolated worker.

That's a real limitation - those programs don't compile, and you have to
restructure. It buys one narrow guarantee, which is the whole point of listing
it: the compiler never silently boxes a capability into an integer slot. When it
can't represent the authority honestly, you get a check-time error instead of a
forgeable handle.
