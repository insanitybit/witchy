# Explicit returns lost proper-tail obligations in ordinary blocks

Status: repaired locally; merge-queue validation pending.

The production `time` typed-error probe reached a fail-closed
`MissingTailObligation` for `time.parse_iso8601` after its short-circuit
ownership join was admitted. The affected source was the early
`return civil(y, mo, da, 0, 0, 0)` inside the `t.length() == 10` branch.

Ordinary effect/value block lowering emitted `Call; Return` without consulting
the checked proper-tail manifest. Only function-final tail lowering used that
manifest. The independent checker rejected the omitted obligation; no unsafe
or non-tail replacement was admitted.

Explicit returns now locate their original checked AST node through the
manifest's structural path and enter the existing tail-expression lowering.
Callable and lambda identities must match. Initializers use the checker's
`Child(0)` convention. Matching never relies on function names or equal-looking
source expressions, and no manifest obligation is removed.

Focused evidence: `explicit_return_in_statement_if_preserves_tail_manifest`
and `explicit_return_in_value_if_preserves_tail_manifest` pass; the original
`typed_errors::time_typed_errors_are_matchable_and_bridge_to_string` passes.
This establishes these lowering cases, not compiler-wide safety or complete
proper-tail coverage.
