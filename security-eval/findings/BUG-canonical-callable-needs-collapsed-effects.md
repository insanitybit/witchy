# Canonical callable needs collapsed destructive effects

## Impact

The body-derived callable checker summarized each parameter with one
`ParameterNeed`. Its join operation collapsed some combinations to a weaker
need, including `Read + Release` to `Read` and `Write + Retain` to `Write`.
Consequently a `CallBorrow` declaration could satisfy the summarized need even
though the body also performed an ownership-changing operation that a borrow
must not authorize.

## Fix

Parameter needs are now exact sets of memory effects. Recursive calls union the
callee's complete set, and ownership conformance checks every member. The
focused regression `call_borrow_cannot_hide_release_behind_a_read_need` pins the
destructive combination.
