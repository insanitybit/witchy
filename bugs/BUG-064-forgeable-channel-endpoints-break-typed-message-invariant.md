# BUG-064: Forgeable channel endpoints break typed-message invariant

- **Severity:** HIGH
- **Status:** OPEN
- **Verified:** 2026-07-09 REPRO on master `1e9626cb`
- **Component:** `std/chan`, RFC-0055 typed channels, compiled backend erasure
- **Found:** 2026-07-05
- **Source:** `security-eval/findings/SEC-046-forgeable-channel-endpoints.md`

## Summary

`std/chan` exposes `Sender(m)` and `Receiver(m)` as ordinary public ADTs around a
raw `Int` channel id. User code outside `std/chan` can destructure a
`Sender(Int)`, recover the id, and construct a `Sender(String)` with the same id.
That violates RFC-0055's pairing invariant: a message no longer necessarily
leaves the channel at the same type it entered.

The compiled backend lowers `__erase` / `__unerase` to identity, so a forged
endpoint can reinterpret an off-type heap value as another type. The security
eval reproduced this as a parity divergence: the interpreter errors when an
`Int` receiver gets a `String`, while the compiled backend silently reads a heap
pointer as an integer and prints it.

This is a release-coherence issue, not a cosmetic API gap. RFC-0055 says the
endpoint pairing invariant makes runtime tags unnecessary; the current language
surface does not enforce that invariant.

## Evidence

- `std/chan.witchy` defines public `type Sender(m): Sender(Int)` and
  `type Receiver(m): Receiver(Int)`.
- `std/chan.witchy` recovers erased values with `__unerase` based on the endpoint
  type parameter.
- `rfcs/0055-channel-message-types.md` relies on endpoint pairing and says user
  code cannot forge an unerase.
- `crates/witchy-lower/src/codegen/builtins.rs` lowers `__erase` and
  `__unerase` as identity on the compiled backend.
- A fresh source probe on master `1e9626cb` still accepts the forbidden
  destructuring/reconstruction shape:

```witchy
import chan
from chan import Sender

async fn main(console: Console):
    let (tx, _rx) = chan.channel(0).await
    match tx:
        Sender(id) ->
            let forged: Sender(String) = Sender(id)
            let _ = forged
            console.print("forged")
```

## Expected

Typed channel endpoints must be non-forgeable, or the erase/unerase boundary must
be checked:

- preferred design fix: make `Sender` and `Receiver` sealed endpoint types so
  only `std/chan` can mint or inspect channel ids;
- acceptable interim hardening: make pattern matching / direct construction of
  runtime-owned handles illegal outside the defining module;
- alternate safety fix: carry runtime type tags through `__erase` / `__unerase`
  and trap on mismatch identically on both backends.

## Acceptance

- The SEC-046 PoC no longer produces a silent compiled success when the
  interpreter errors.
- RFC-0055's pairing invariant is enforced by code, not only by convention in
  `std/chan`.
- A regression test covers endpoint-forgery attempts on both backends.
- The fix reconciles with the active channel-runtime work; as of 2026-07-09,
  `fix/chan-delegates-task-core` is already editing `std/chan.witchy`, so this
  bug record deliberately does not change channel code.
