# Nested string extent lanes and predecessor-local header facts

Status: lowering corrections, focused executable parity passed; queue validation pending.

Two fail-closed refusals appeared in the post-URL integration census:

- `hello`: `InvalidOwnership { function: FunctionId(1), object: ValueId(20) }`.
  The inner `if` result had authenticated count and capacity lanes 15 and 16.
  Lowering replaced its cached extent with a length-header load, then the outer
  match forwarded that load twice (with a distinct copy for capacity). The
  checker correctly retained capacity 16, which was absent from the outer edge.
- `wrap`: `ValueDoesNotDominateUse { function: FunctionId(1), value: ValueId(166), block: BlockId(4) }`.
  Absorbed branch caches supplied a predecessor-local string header load to a
  different predecessor while planning binding lanes.

The corrections retain authenticated result-phi count and capacity identities,
and derive string binding lanes in their actual predecessor rather than using
an absorbed branch's header cache. They do not change checker rules, conflate
list length with capacity, materialize ownership to avoid a refusal, or add a
raw fallback.

Focused evidence with the rebuilt CLI:

```sh
TARGET=/private/tmp/witchy-rfc0165-drop-worklist-target
"$TARGET/debug/witchy" parity examples/hello/src/hello.witchy
"$TARGET/debug/witchy" parity examples/wrap/src/wrap.witchy
```

Both pass: three and five output lines respectively. This is interpreter/Wasm
parity, not a separate SafeWIR-oracle result or compiler-wide soundness evidence.
The last full classified census remains 133/163 checking, 76 runnable agreements,
zero disagreements, measured before these corrections. No new total is claimed.
