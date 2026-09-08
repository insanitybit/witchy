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
It landed through the merge queue at 2026-09-08 12:21:12 UTC, separately
from this admission extension.

## Source and shared-storage integration

The actual production-source gap above is now covered separately by
`tests/misc/rfc0165_nullable_source.rs`. Its checked source includes empty and
present `Option(String)` and user sums, nested `Holder(None)` values, and a
tuple with the same scalar/reference field shape as the option. A Bool-parameter
function joins present and absent branches, exercised with both arguments. Source
interpreter, canonical oracle, and receipt-bound Wasm produce the eight expected
lines; both heap audits report zero leaks.

The layout resolver and source ABI checker require exactly nullable reference
fields for tagged shapes and ordinary owned fields for untagged shapes. The
same-shaped tuple cannot substitute for the sum's canonical layout.

Ordinary aggregate fields now share references explicitly rather than copying
every variant field, which would load an absent inactive payload. The backend
retains before `Store(CloneShared)` publishes its edge, and its independent
plan states the same sequence. A focused oracle/admission/Wasm probe verifies
that an external child reference survives parent destruction, for both owned
and nullable field layouts, and that the oracle eventually releases everything.
Static source literals are explicitly recognized by release placement as
immortal when cloned; the memory checker still authenticates their segments.

A final read-only Luna review of the integrated sharing and source changes
found no concrete issue within its scope: RC balance, transfer selection,
nullable destruction, authenticated static data, and list copy-on-write paths.
It was not a compiler-wide soundness audit and ran no additional tests.

Focused commands use the existing absolute target:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target taskpolicy -c utility cargo test --test misc rfc0165_nullable_source -- --nocapture
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target taskpolicy -c utility cargo test -p witchy-wir --lib --features native shared_store
```

Each command passed one focused test. The source log is
`/private/tmp/rfc0165-nullable-source-join.log`; the shared-store log is
`/private/tmp/rfc0165-shared-store.log`.

## Remaining

Opaque nominal bindings still need checked-source authentication and recursive/generic source
lowering. Their admission seal remains; this is not recursive source coverage.

Compiler-wide memory safety remains unestablished.
