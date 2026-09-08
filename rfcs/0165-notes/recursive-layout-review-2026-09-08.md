# RFC-0165 recursive nominal/layout transport review (read-only)

Scope: `layout/specialized.rs`, `layout/transport.rs`, canonical SafeWIR initial check, the recursive transport tests, and the recursive-types contract. No source edits or execution.

## Result

No concrete bypass or missing fail-closed condition was found in the reviewed preparatory slice. In particular, an artifact cannot hide a reachable opaque nominal by omitting its binding or target, nor obtain normal canonical admission after transport succeeds.

## Evidence reviewed

- `OpaqueNominal` is identity-only, `bind_nominal` requires an extant non-opaque target and permits no rebinding, and `resolve_nominal` is an explicit one-step operation (`crates/witchy-wir/src/layout/specialized.rs:464-466`, `:988-1027`). Inline construction rejects opaque nominal children (`crates/witchy-wir/src/layout/specialized.rs:2606-2610`).
- Version-2 export walks ordinary descriptor dependencies acyclically, then follows every reachable opaque nominal to require and include its binding target; it serializes a sorted binding table (`crates/witchy-wir/src/layout/transport.rs:36-73`, `:87-115`).
- Decode imports descriptors, validates strictly sorted bindings through `bind_nominal`, then rebuilds from roots and requires byte-for-byte canonical equality. This rejects dangling/missing/unused/reordered transport state rather than accepting a partial graph (`crates/witchy-wir/src/layout/transport.rs:120-185`). Descriptor cycles are separately refused during traversal (`crates/witchy-wir/src/layout/transport.rs:486-527`).
- The initial canonical check decodes the candidate's canonical bytes and rejects if the reconstructed layout interner contains *any* `OpaqueNominal`, before static data, signatures, imports, functions, or contracts are processed (`crates/witchy-wir/src/canonical_safewir/check.rs:242-257`). A concrete body cannot bypass that check because the reachable identity descriptor itself remains in the reconstructed bundle.
- The targeted tests cover self recursion, mutual recursion, unbound/rebound/inline refusal, binding-dependent bytes, malformed missing-binding bytes, and the production-authority refusal (`crates/witchy-wir/tests/recursive_layout_transport.rs:12-95`).

## Boundary retained

The contract correctly records an intentionally absent boundary: candidate/source admission has not compared a nominal key and concrete body against checked nominal fields using memoized `(source type, candidate layout)` pairs (`rfcs/0165-notes/recursive-types-contract.md:19-25`). That is not an immediate authority bypass because the checked production path rejects every transported opaque nominal. It must remain blocked until ownership/destruction and source binding land together, as the same note states (`rfcs/0165-notes/recursive-types-contract.md:40-63`).

This review makes no recursive-source support or safety claim and does not assess unreviewed call paths outside the named files.

## Addendum: canonical interpreter iterative release

No concrete defect found in the added interpreter slice. `Engine::release` replaces recursive child release with a local LIFO worklist (`crates/witchy-wir/src/canonical_interpreter.rs:1527-1558`). It removes a zero-count object before enqueueing each stored child, pushes the children in reverse insertion order to retain the old depth-first order, and deliberately does not deduplicate repeated child edges. That is required: each stored edge represents one owned token and must decrement a shared child's count once.

The supplied green tests cover a 25,000-object chain, two edges to one shared child with an external owner, and static-child/underflow behavior (`crates/witchy-wir/src/canonical_interpreter.rs:1899-1936`). The diff preserves the prior static-data no-op and checked-subtraction underflow behavior before a child is enqueued. This is interpreter-only infrastructure; it neither changes the production `OpaqueNominal` refusal nor provides backend worklist or recursive-source authority.
