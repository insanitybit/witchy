# `unreachable`

```
fn digit(byte: U64) -> U64:
    if byte >= 48 && byte <= 57:
        return byte - 48
    unreachable
```

A statement that stops the run with a trap (Wasm `unreachable`). It is
Witchy 1's `safe_unreachable` (rfcs/0177, section 6): defined failure, never an
assumption the compiler may use. The shorter spelling is mine; Witchy 2 has no
`unsafe_unreachable`. Under a numeric contract it is also RFC 0177's `prove
unreachable`: the proof must show the path is never taken.

What the checker does with it:

- It ends the path, like `return`: a function need not return after it, and a
  `match` or `if` used as a value may leave by it.
- Owners still live there are abandoned, not released: the run is over, so
  nothing can see them. No code is emitted for them.
- In a function proved by the [numeric control-flow walk](../src/compiler/scalar_vc/flow.rs), it is an
  obligation: what is known on the way to it must be contradictory (goal
  `False`). `proved.wy` passes because `requires value % 2 == 0` rules out the
  odd branch; `reachable.refused.wy`, without it, is refused. So a proved
  function never traps there.
- A function whose contract is proved by the memory or record model refuses it
  for now (`memory-contract.refused.wy`).
- A function without a contract that is walked for its calls' goals treats it
  as the end of the path: a defined trap, nothing to prove.

The engine already admitted the instruction, so nothing in the host changed.

`Vec.release_empty` and `HashMap.release` use it: called on a value that still
holds elements, they used to retry until the instruction budget ran out, and
now trap at once.

Fixtures: `tests/fixtures/unreachable/` (`paths.wy` runs; `release-full.traps.wy`
traps; `proved.wy`; `reachable.refused.wy`; `memory-contract.refused.wy`).

Trusted: the checker rule (tested, not proved), the emitter's one byte, and the
numeric walk's reading of it (one `False` goal per `unreachable`), as for its
other statements.
