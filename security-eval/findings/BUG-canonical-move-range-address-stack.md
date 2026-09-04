# BUG: canonical MoveRange consumed its checked address

The RFC-0165 canonical backend emitted `MoveRange` address arithmetic, consumed
the resulting `i64` in an overflow comparison, and then emitted `i32.wrap_i64`
without reconstructing the address. The independent exact-admission plan made
the same mistake, so correspondence alone did not detect it; `wasmparser`
rejected the module with `type mismatch: expected i64 but nothing on stack`.

The same focused probe exposed a second control defect: the zero-count range
loop used `br 1` from inside an `if`, targeting the loop rather than its outer
exit block and hanging forever. Both encoder and plan now use depth 2.

The defect was reached by the canonical list-for vertical slice while moving a
pointer-bearing list prefix during `list.push`. Both the encoder and independent
plan now recompute the address after the overflow trap before narrowing it.
This preserves exact-plan independence while leaving Wasm structural validation
as an additional mandatory admission check.
