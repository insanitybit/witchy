# Canonical host contracts allowed reference-bearing generic Value roles

Status: local hardening; compiler-wide exploitability not established.

The canonical host checker rejected linear references in scalar call arguments,
but its reference-like predicate omitted capabilities. An import catalog entry
declaring `Value(Capability(K))` could therefore pass both contract and argument
checking instead of using the dedicated capability role. Result-role checking
also allowed `Value(Ref(...))`, `Value(Gc(...))`, and `Value(Callable(...))` without
an explicit ownership or result-lifetime role.

The authenticated catalog limits who can introduce these declarations; this is
not evidence that arbitrary source can forge a catalog or mint a capability.
The checker nevertheless needs to reject such declarations independently, so a
catalog extension cannot accidentally bypass the dedicated host rules.

Fix: generic host Value parameter and result roles accept scalar values only.
Capabilities require the dedicated capability role. Reference-bearing results
remain unsupported until an explicit checked ownership/lifetime role exists.
Focused tests reject generic reference roles and preserve the dedicated
capability parameter path.
