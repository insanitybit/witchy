# Nullable fixed-field admission

This extends the sealed operations slice with initial/final validation,
mechanical lowering, exact-byte admission, and execution of fixed nullable
record fields. Opaque nominal values, standalone nullable slots, and nullable
collection/range carriers remain refused.

Two focused WIR probes pass: a present child returns `42` and leaves no oracle
allocation live; an absent child traps in both oracle and Wasm. Changing the
encoded `i64.const 42` to `43` preserves Wasm structural validity but fails
exact-byte admission. These probes use the existing test-source witness API.
They do not establish production checked-source binding or recursive source
coverage. A separate source-lowering track must supply that evidence.

The catalog rule is structural: a nullable value slot must occur as a field of
a packed record or tuple, and cannot simultaneously be a collection element.
A negative candidate exercises a nullable slot referenced by both a record
and a list; the fixed-field use cannot authorize the unsupported list.

Read-only backend review found no catalog authority bypass in this scope and
identified the test-witness limitation above. Production witness validation is
unchanged, not bypassed or replaced by these probes.

The preceding queued slice hit exhaustive lowerer matches in
`canonical_owned.rs`. Commit `2560a68e4` supplies initialization uses and tracks
the owned token produced by `ClonePresent`; its focused lowerer test passes.
It was resubmitted separately from this admission extension.

## Next coupled work

- Lower inactive source sum fields through `InitNullableOwned` and nullable
  reads through `ClonePresent`, then compare source, oracle, and admitted Wasm.
- Cover mixed absent/present branch joins with an executable source probe.
- Implement backend `Store(CloneShared)` with an explicit retain and matching
  independently authored plan before relying on shared recursive children.
  It currently fails closed; recursive layout expansion is not a substitute.
- Authenticate opaque nominal bindings and support recursive/generic source
  layouts without removing their admission seal prematurely.

Compiler-wide memory safety remains unestablished.
