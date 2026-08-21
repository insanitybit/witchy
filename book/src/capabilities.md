# Capabilities: The Heart of witchy

Effect-free functions are easy to reason about, but an ordinary function type
does not promise purity merely because it has no capability parameters. Code
may receive authority directly as a capability value or transitively as opaque
behavior delegated through a callback.

These chapters are about the other kind of code: the code that does need host
authority, and what it costs to keep that authority visible instead of
ambient.

The chapters develop one rule:

> **Root authority enters a program in exactly one place - the parameters of
> `main` - and flows onward only through values.**

- [**Authority as a Value**](capabilities-authority.md) - what a capability *is*,
  and why it can't be forged.
- [**Narrowing**](capabilities-narrowing.md) - handing out less
  power than you hold.
- [**Optional and Conditional Capabilities**](capabilities-optional.md) - authority
  a program may or may not be granted.
- [**The Sandbox**](capabilities-sandbox.md) - turning "the types say so" into
  "the VM enforces it."

witchy makes this object-capability model part of the language and checks it in
the type system. Capability-typed parameters state directly possessed
authority; ordinary callbacks expose delegated behavior without disclosing
their captured capabilities. The checked `pure fn` contract is the explicit
way to require effect-free invocation.
