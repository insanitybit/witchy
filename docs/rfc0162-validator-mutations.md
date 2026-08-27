# RFC-0162 validator mutation evidence

The schema-1 mutation matrix is
[`rfcs/0162-validator-mutations.schema1.json`](../rfcs/0162-validator-mutations.schema1.json).
It covers every stable `RuleId` with one named semantic weakening, one uniquely
resolved cargo-mutants mutation, and one focused test command.

Run the reproducible harness from the repository root:

```sh
./scripts/rfc0162-validator-mutations.sh check
./scripts/rfc0162-validator-mutations.sh run SWIR-OWN-002
./scripts/rfc0162-validator-mutations.sh run-all
./scripts/rfc0162-validator-mutations.sh verify-observed
```

The harness pins cargo-mutants 27.1.0 and authenticates the complete validator,
the independent callable-contract checker, and the stable RuleId catalog with
matrix-recorded SHA-256 digests. It uses the isolated worktree target, caps
Cargo and the jobserver to one task, and runs each mutation in place so it does
not cold-copy the multi-gigabyte target. It checks the selected source checksum
after every invocation. A missing or renamed selector, a selector that resolves to
zero or multiple mutants, a failed baseline, a surviving mutant, a timeout, an
unviable mutation, a nonzero cargo-mutants exit, or a source-restoration mismatch
is a hard failure. Per-run raw logs and `outcomes.json` files remain under
`target/rfc0162-validator-mutations/`.

## Evidence boundary

Fifteen rows are current report-mode validator rules and receive mutation-kill
credit. Five rows are only report-mode sentinels for production authority that
does not yet exist at this boundary:

- `SWIR-MOD-001` checked-source entry authentication;
- `SWIR-RNG-002` final exact range-source identity and dominance;
- `SWIR-HOST-001` authenticated production host ABI contracts;
- `SWIR-KERN-001` the sealed raw-memory kernel boundary; and
- `SWIR-ENC-001` sole `ValidatedWirModule` encoder authority.

Those five rows can record that the current sentinel mutation was caught, but
`acceptance_credit` remains false until the production boundary replaces the
attested candidate fields and has its own killed mutation. Consequently this
track always records `acceptance_complete: false`; it cannot close RFC-0162 by
itself.

The recorded cargo-mutants 27.1.0 execution against the exact source digests in
the matrix observed 20 `CaughtMutant` outcomes, zero survivors, zero timeouts,
and zero unviable selected mutations. Fifteen kills receive current report-mode
acceptance credit. The five sentinel-only kills do not. `verify-observed` checks
that this recorded summary is internally consistent; only `run-all` observes
the mutations again.

During harness calibration, an unviable synthesized default-object mutation and
a surviving duplicate fact-budget check were rejected rather than counted. The
selected matrix replaces them with generation-mismatch and initial-state-fact
weakenings that were actually caught. After stacking on the call-contract track,
broad provenance and loop-budget fixtures plus an end-to-end callable-contract
fixture survived because independent checks masked the selected weakening. They
were replaced by single-defect provenance, stable-header operation-count, and
direct call-analysis fixtures. One host-contention build timeout corrupted the
isolated incremental target and made the immediate retry unviable; both attempts
were rejected, the affected package artifacts were rebuilt, and the final
`run-all` observed all 20 selected mutations as caught. Adversarial review then
identified two selections that only changed rejection diagnostics: deleting the
unknown-result match arm and weakening typed-fill access. They were replaced by
admitting call-validation and shared bulk-copy-destination mutations, each of
which was observed caught by a focused single-defect test.

## Landing order

This evidence is stacked after call-contract commit `fd8a01d62`. The matrix
authenticates and selects mutations from both final source files. Any later
rewrite invalidates a source digest or line-pinned selector, causing `check` to
fail closed.

The external fixtures in
`crates/witchy-wir/tests/rfc0162_mutation_evidence.rs` are deliberately narrow.
They isolate provenance, loop convergence, scalar-to-reference separation,
range generation, host buffer direction, and initial fact charging. The
crate-private test in `crates/witchy-wir/src/rfc0162_mutation_evidence_tests.rs`
observes callable-contract recomputation directly so downstream validation
cannot mask its deletion. A test-only module declaration in `lib.rs` is the only
hook; neither validator nor call-analysis implementation is changed.
