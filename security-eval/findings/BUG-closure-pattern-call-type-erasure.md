# Callee-only pattern captures lost their checked callable type

Observed on the canonical task integration, 2026-09-10.

An ordinary checked program can destructure `Handler(cont)` and return
`fn(value: Int): cont(value)`. The closure witness refused `cont` because the
pattern binding has no expression-type entry and a named `Call` has no `Var`
child. The checked table retained the call's result, but not its lexical
callee type. The same omission blocked the source scheduler's continuations.

The type checker now records the lexical callee alongside each persistent
named-call result and resolves it with the final substitution. Only concrete
function types enter the final table. Transient inference uses the existing
recording suppression, so temporary AST addresses grant no facts. The closure
witness consumes the exact checked call node and resolves the binding in its
lexical scope; it does not derive a signature from argument values or look up
a same-named global function.

The focused type test shadows a differently typed global `cont` and rejects
an unrelated same-spelled call node. The executable regression constructs,
captures, returns, calls twice, encodes, and releases the callable through all
three execution legs. This is a false fail-closed refusal, not a demonstrated
memory-safety bypass.
