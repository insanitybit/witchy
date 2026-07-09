# Capabilities: The Heart of witchy

Everything so far has been ordinary, pleasant, and *pure*. The functions you
wrote couldn't print, read a file, or touch the network — and you could tell,
because nothing in their signatures granted that power.

This chapter is about the other kind of code: the parts of a program that
genuinely need to affect the world, and how witchy makes that authority
explicit, bounded, auditable, and enforceable.

The whole model rests on one rule, which we'll spend four sections unpacking:

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

If you've used object-capability systems before, this will feel familiar — it's
the ocap idea, made the default and checked by the type system. If you haven't,
the punchline is: "what can this code do?" stops being a question you answer by
auditing and starts being one you answer by *reading a type*.
