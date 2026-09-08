# RFC-0165 source-shape / recursive-layout review

Review target: `aa1539c56791e95a11134c0d2fe616781ba255d7` (`aa1539c56`).
Review scope: source-shape acceptance, recursive refusal, deeper acyclic record
admission, qualified roots, unsupported/capability/packed acceptance, and
authority widening. This is a read-only review; no builds or tests were run by
the reviewer.

## Findings

No concrete security or correctness finding in the requested scope.

### Recursion termination

`canonical_aggregates::definition_fields_ok` carries an active nominal-name
set and treats an already-active non-packed definition as a bounded recursive
edge; packed active recursion is rejected. The source-shape walk therefore
returns a finite shape without expanding the cycle.

`LayoutProjection::visit_type` has separate active and completed sets. An
active edge returns without descending, while a completed type is skipped only
after its full field walk succeeded. It records roots after the finite walk,
so self and mutual recursion do not cause unbounded source traversal.

`intern_v1_layout_checked` carries an active unqualified `Type` set and refuses
an active edge with `recursive nominal layout ... requires nullable owned-slot
lowering` before following the edge again. The lowerer therefore does not
claim recursive-source success. The layout transport also requires a
finite dependency-first descriptor graph, so the downstream source-layout
matcher is not exposed to an authenticated cyclic layout graph by this change.

### Qualified roots

`Type::unqualified` removes only ownership/immutability/borrow qualifiers;
it does not strip module qualification embedded in nominal names. Using the
unqualified physical type as the projection/interner key is therefore
consistent with the runtime representation. The projection also keys both the
bare and entry-qualified definition spelling. I found no lost physical root
or cross-module alias that widens authority.

### Unsupported, capability, and packed shapes

The shape predicate rejects generic definitions, capability definitions, and
empty definitions. Builtins have exact arity checks for `List`, `Option`, and
`Result`; unsupported forms do not become aggregate shapes. `Dir`/`Console`/
`Env` remain capability cases in layout projection rather than linear-memory
roots.

Packed scalar definitions can be collected as a layout root by the projection
fallback. In the specific `source_value_type` ABI path inspected here,
canonical source value typing rejects `shape.packed` and falls through to the
scalar path, which refuses the nominal type. I did not audit the foundation
packed-scalar lowering path and make no global claim that packed scalars are
unsupported. This is an unverified context-specific observation, not
authority widening or a finding against this bounded change.

## Evidence and residual risk

The integration handoff reports three source-shape tests, three
layout-projection tests, and two real-pipeline probes green. I did not rerun
them under this read-only review. The recursive pipeline probe is correctly a
precise refusal probe; it does not establish recursive source success. The
remaining future work is nullable owned-slot lowering, not a change required
for this review target.
