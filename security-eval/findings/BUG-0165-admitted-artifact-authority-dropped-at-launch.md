# BUG-0165: admitted artifact authority is dropped before runtime launch

## Status

Open. This is a blocker for the RFC-0165 production cutover, not evidence that
the current development-only canonical route is already exploitable in
production.

## Finding

Canonical admission produces an artifact whose bytes are bound to validated
SafeWIR authority, but `AdmittedCanonicalArtifact::wasm()` exposes those bytes
as an ordinary slice. Runtime launch accepts arbitrary byte slices and may run
Binaryen before execution. The runtime therefore neither proves that its input
is an admitted artifact nor preserves the exact admitted bytes.

Exact-byte admission only establishes a security boundary if every cache,
publish, and launch consumer retains artifact authority. Mutating or replacing
the detached bytes after admission invalidates that boundary even when the
result remains structurally valid Wasm.

## Required repair

- Production launch/cache/publish APIs consume an admitted-artifact type, not
  caller-supplied bytes.
- The admitted artifact retains the exact validated-module and byte identity.
- No Binaryen or other byte transformation runs after admission. If a transform
  is retained, its result must return through SafeWIR validation and exact-byte
  admission before it can execute.
- Raw-byte runtime entry points are test-only or explicitly outside the Witchy
  compiler safety claim and unreachable from production compilation.

## Acceptance evidence

- A focused compile-time or reachability test shows canonical production output
  cannot call a raw-byte launch API.
- A mutation/substitution test cannot launch bytes under the original artifact
  authority.
- The ordinary admitted scalar/list program executes through the artifact-bound
  launch path with byte identity unchanged.

