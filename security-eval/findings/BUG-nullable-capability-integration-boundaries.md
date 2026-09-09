# Nullable capability integration boundaries

Status: pre-landing findings corrected; focused acceptance passed; queue pending.

Bounded review of the nullable-capability core found that initial host checking
did not include `NullableCapability` among reference-like types prohibited in
generic host value roles. Backend scalar encoding still rejected these values,
so this was an incomplete initial boundary, not demonstrated production emission
of an unauthorized host call. The initial checker now rejects nullable generic
parameters, generic results, and direct capability arguments. An explicit unwrap
is required before a nullable value can satisfy a capability role.

Integration also found two fail-closed gaps:

- Body-derived callable summaries classified capability returns as unknown.
  They now preserve the exact capability or nullable-capability value type as
  a non-memory result. This does not enable scalar arithmetic on capabilities.
- Byte decoding recognized the four new reference operations but expected-plan
  construction did not. Explicit mappings now require null externref creation,
  reference copy, null testing with inversion, and trapping non-null unwrap.

The focused launch family
`rfc0165_admitted_launch::nullable_capabilities` passes two tests, including
independently expected `present` and `absent` output from the source interpreter,
SafeWIR oracle, and admitted Wasm. The source-negative cases reject integer
brand construction and use of an optional capability without unwrapping.

The reviewer found no checked-source reproducer for an earlier nominal-brand
concern: checked source already distinguishes a brand from its base capability.
ABI representation equality does not authorize a source-level coercion. Exact
nominal equality must not replace the canonical checker's valid coercion rules.

Additional focused evidence: the real `branded_caps` example agrees between the
source interpreter and compiled Wasm; the oracle directly traps absent unwrap
and rejects a mismatched nullable kind; generic host parameter/result roles and
direct capability roles reject nullable values. Tail-site classification uses
only the resolved `__capop.` marker and the fixed catalog in both source and
lowering, with a test preserving a recursive guest function named `read`.

These observations do not establish compiler-wide memory safety. Queue landing
and a fresh classified full corpus remain separate acceptance items.
