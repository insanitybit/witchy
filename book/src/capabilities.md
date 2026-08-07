# Capabilities: The Heart of witchy

Pure functions cannot print, read a file, or touch the network because their
signatures grant none of those capabilities. This chapter covers code that does
need host authority and how witchy bounds and enforces it.

The chapters develop one rule:

> **Authority enters a program in exactly one place - the parameters of `main` -
> and flows onward only as function arguments.**

- [**Authority as a Value**](capabilities-authority.md) - what a capability *is*,
  and why it can't be forged.
- [**Narrowing**](capabilities-narrowing.md) - handing out less
  power than you hold.
- [**Optional and Conditional Capabilities**](capabilities-optional.md) - authority
  a program may or may not be granted.
- [**The Sandbox**](capabilities-sandbox.md) - turning "the types say so" into
  "the VM enforces it."

witchy makes this object-capability model part of the language and checks it in
the type system. Capability-typed parameters state the authority a function can
exercise.
