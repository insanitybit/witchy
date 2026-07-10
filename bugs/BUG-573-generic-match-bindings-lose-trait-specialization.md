# BUG-573: Generic match bindings lose nested trait specialization

Status: FIXED
Severity: HIGH
Component: `witchy-types`, trait monomorphization, RFC-0046

## Summary

A bounded trait call on a generic constructor-pattern binding could name a
generic no-fallback implementation instead of specializing it for the binding's
concrete type. Direct payloads happened to work, but a nested blanket
implementation such as `Show for List(a)` exposed the failure. For example,
matching `Result(List(P), String)` and calling `show(value)` in the `Ok` arm
could leave a call to `Show__List__show`; that template is removed after
monomorphization, so the linked program failed with `call to unknown function`.

The substitution-directed call-renaming pass marked pattern names as untyped
locals. When a function had multiple bounds for the same trait, it therefore
could not use the match arm's receiver type to choose and specialize the right
implementation. `std/show` and `std/reflect` masked parts of the defect by
moving `Option` and `Result` payloads through temporary lists and loops.

## Resolution

Call renaming now seeds constructor-pattern bindings from the constructor's
field types and the active generic substitution before marking any unresolved
names as locals. Nested generic `Some`, `Ok`, and `Err` payloads consequently
retain enough receiver evidence to specialize bounded `Show` and `Reflect`
calls.

The standard-library workarounds are removed. A differential regression covers
direct and nested generic payloads on the interpreter and compiled backend, and
the existing nested-container protocol regression remains green.
