# Immutable bindings reuse a consumed reference token

## Reproducer

```witchy
fn main(console: Console):
    var words = ["root"]
    let snapshot = words
    words.push("tail")
    console.print("${snapshot} ${words}")
```

The canonical route rejects this ordinary value-semantic program with
`InitialValidation(Safety(UseAfterConsume { function: FunctionId(0), object: ValueId(6) }))`.
This was observed on the owned-prefix candidate's rebuilt CLI. It is a
fail-closed lowering defect, not evidence of an admitted use-after-free.

`lower_let_value` previously returned a bound linear-memory reference unchanged
for every immutable binding. Both names therefore denoted the same affine
token. List growth copies the prefix and retires the old receiver token, making
the snapshot's later read invalid. The defect is independent of the new
owned-prefix containment rule: the existing live-object check rejects it.

The correction gives every bound reference initializer its own `CloneRef`
token, including immutable initializers. Existing mutable static-string
materialization stays in place. Growth can then retire the source binding's
token without freeing the snapshot's storage. No checker rule is relaxed.

Focused regression: `owned_binding_snapshots_survive_source_list_growth` in
`tests/misc/rfc0087_uniform_var.rs`, covering scalar and string lists with
independently specified outputs. Four snapshot/projected-writeback tests pass
in `/tmp/witchy-owned-alias-source.log`; 683/683 native WIR tests pass with no
skips in `/tmp/witchy-owned-alias-native-wir.log`. The rebuilt CLI agrees on
hello, wrap, jq, and the original nested-list snapshot probe. Merge evidence
remains pending.

Retained pointer-list reads also exposed exact-SSA-keyed initialization
lookup. Shared loads now consult the existing allocation identity for stored
slots and initialized prefixes, while checking receiver liveness, bounds,
managed child storage, and ownership as before. Retaining a recorded child
demotes every corresponding slot fact. Unique moves still require the
original exact-key and unique-owner judgments. New native tests cover a load
after the original root token is dropped, invalid prefix evidence, and
rejection of initialization inferred merely from matching layouts.
