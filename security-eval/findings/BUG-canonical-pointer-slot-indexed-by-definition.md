# Canonical pointer slots were indexed by definition identity

The canonical SafeWIR checker originally keyed an initialized pointer-bearing
collection slot by `(container, index ValueId)`. Two independently derived
constant indices with the same numeric value therefore denoted different slots.
A `List(String)` literal initialized element 1 with one constant definition, but
the later expression `xs[1]` used a new definition and failed closed as an
uninitialized-reference load.

The checker now uses the independently evaluated constant value as the slot
identity when available, while retaining SSA definition identity for dynamic
indices. CFG remapping changes only dynamic identities. The focused
`list_string_index_reaches_final_safewir_validation` probe covers the source
reproducer through final SafeWIR validation.
