# Canonical SafeWIR affine boundary accounting

Baseline: `72dea087c`, canonical development route. Compiler-wide memory safety
is not established. These are checker defects, not evidence of a production
exploit through every frontend and backend restriction.

- Return checked each lane independently. Repeating one owned SSA reference in
  two result lanes could claim two fresh or shared references without CloneRef.
- Owned call parameters assumed ReferenceCounted storage in the callee, while
  caller Move checked only ownership. Bump, Scratch, or unknown-class references
  could acquire authority to operate on a nonexistent RC header. Shared result
  contracts had the same class mismatch. Current backend class restrictions
  limit reachability but do not establish the checker rule.
- Tail exits did not check remaining affine obligations. Result type equality
  also did not enforce the ownership/storage result contract forwarded by a tail
  edge. Tail lowering cannot insert caller cleanup after the edge.
- CFG joins intersected object and heap-slot maps as if owned tokens were
  disposable analysis facts. An incoming edge could lose a release obligation.
  Lost release obligations establish a leak/resource defect; stronger temporal
  consequences require a separate executable reproducer.

Repair: reject duplicate transferred return tokens, authenticate RC class at
owned ABI boundaries, check tail exits and result contracts, and reject joins
that discard live RC tokens or disagree on owned heap slots. Compatible phi
transfers remain supported; differing heap ownership states fail closed until
there is an explicit ownership join, not a guessed alias conclusion.

Focused tests are named `affine_boundary_*` in the canonical memory checker.
They include positive RC moves, explicit CloneRef result lanes, consumed-token
joins, and tail exits after cleanup. Test results must be recorded separately;
the presence of these tests is not evidence that they have run.
