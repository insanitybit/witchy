# Capabilities: The Heart of witchy

Everything so far has been ordinary, pleasant, and *pure*. The functions you
wrote couldn't print, read a file, or touch the network — and you could tell,
because nothing in their signatures granted that power.

This chapter is about the other kind of code: the parts of a program that
genuinely need to affect the world, and how witchy makes that authority
explicit, bounded, auditable, and enforceable.

Four sections develop one rule:

> **Authority enters a program in exactly one place — the parameters of `main` —
> and flows onward only as function arguments.**

- [**Authority as a Value**](capabilities-authority.md) — what a capability *is*,
  and why it can't be forged.
- [**Narrowing and Attenuation**](capabilities-narrowing.md) — handing out less
  power than you hold.
- [**Optional and Conditional Capabilities**](capabilities-optional.md) — authority
  a program may or may not be granted.
- [**The Sandbox**](capabilities-sandbox.md) — turning "the types say so" into
  "the VM enforces it."

This is an object-capability system made the language default and checked by the
type system. A function's capability-typed parameters state the authority it can
exercise.
