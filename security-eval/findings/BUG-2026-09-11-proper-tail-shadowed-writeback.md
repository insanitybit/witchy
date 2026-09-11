# Proper-tail writeback confuses a shadowed local with the caller parameter

Status: confirmed in the uncommitted checked-signature tail-manifest change on
`impl/rfc0165-runtime-parity`, based on `e822fe72c`. Not submitted for landing.

## Reproducer

```witchy
fn bump(var n: Int) -> Int:
    n = n + 2
    n * 10

fn apply(operation: fn(var Int) -> Int, var n: Int) -> Int:
    if true:
        var n = 10
        operation(n)
    else:
        0

fn main(console: Console):
    var n = 1
    let answer = apply(bump, n)
    console.print("${n} ${answer}")
```

Command run from `/private/tmp/witchy-rfc0165-nested-owners`:

```sh
/private/tmp/witchy-rfc0165-reflect-specialization-target/debug/witchy \
  parity /tmp/witchy-tail-shadow-writeback.witchy
```

Result: exit 3; interpreter output `["1 120"]`, compiled output `["12 120"]`.
Captured in `/tmp/witchy-tail-shadow-writeback.log`.

## Cause and correction requirement

`direct_tail_envelope_is_forwarded` in `crates/witchy-types/src/typeck.rs`
compares a call argument's name with the caller's `var` parameter name.
Matching spelling and checked function types do not establish lexical binding
identity. Here the call updates the inner local, but the tail manifest permits
its returned writeback lane to replace the outer parameter.

Forwarded lanes must identify the original parameter binding, not a shadowing
binding. The correction must preserve legitimate indirect proper-tail calls,
reject reordered or incompatible writeback envelopes, and retain ordinary-call
writeback to the inner local in this program. Do not suppress proper-tail
manifest errors or weaken final validation to admit this case.

The existing executable regression
`rfc0087_uniform_var::function_value_conventions_are_identity_and_preserve_writeback`
passes on this candidate (1 passed, 2.87 seconds), as do eight focused manifest
tests. Those results do not cover this shadowing case and are insufficient to
submit the patch. This is confirmed semantic miscompilation; this reproducer
alone does not demonstrate a memory-safety violation.
