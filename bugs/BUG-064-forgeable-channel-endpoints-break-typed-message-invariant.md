# BUG-064: Forgeable channel endpoints break typed-message invariant

- **Severity:** HIGH
- **Status:** FIXED
- **Verified:** 2026-07-09 SOURCE+TEST on branch fix/bug064-sealed-chan-endpoints
- **Component:** `std/chan`, RFC-0055 typed channels, compiled backend erasure
- **Found:** 2026-07-05
- **Source:** `security-eval/findings/SEC-046-forgeable-channel-endpoints.md`

## Summary

`std/chan` used to expose `Sender(m)` and `Receiver(m)` as ordinary public ADTs
around a raw `Int` channel id. User code outside `std/chan` could destructure a
`Sender(Int)`, recover the id, and construct a `Sender(String)` with the same
id. That violated RFC-0055's pairing invariant: a message no longer necessarily
left the channel at the same type it entered.

`Sender` and `Receiver` are now `sealed type`s. Per RFC-0065, external code may
still name, pass, and inspect endpoint values, but it cannot construct endpoint
values from raw channel ids. That closes the unsafe operation: leaking the id no
longer lets user code rebuild the id at a different message type and make
`__unerase` lie.

The compiled backend lowers `__erase` / `__unerase` to identity, so a forged
endpoint can reinterpret an off-type heap value as another type. The security
eval reproduced this as a parity divergence: the interpreter errors when an
`Int` receiver gets a `String`, while the compiled backend silently reads a heap
pointer as an integer and prints it.

This is a release-coherence issue, not a cosmetic API gap. RFC-0055 says the
endpoint pairing invariant makes runtime tags unnecessary; the current language
surface does not enforce that invariant.

## Evidence

- `std/chan.witchy` defines `sealed type Sender(m): Sender(Int)` and
  `sealed type Receiver(m): Receiver(Int)`.
- `std/chan.witchy` recovers erased values with `__unerase` based on the endpoint
  type parameter.
- `rfcs/0055-channel-message-types.md` relies on endpoint pairing and says user
  code cannot forge an unerase.
- `crates/witchy-lower/src/codegen/builtins.rs` lowers `__erase` and
  `__unerase` as identity on the compiled backend.
- A source probe on master `1e9626cb` accepted the forbidden
  destructuring/reconstruction shape; after the fix, the same shape is rejected
  at link time as sealed construction:

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

## Fixed

Typed channel endpoints are non-forgeable at the operation that matters for the
type invariant:

- `std/chan` is the only module that can mint `Sender(m)` / `Receiver(m)` from
  a raw executor channel id;
- a caller cannot convert a `Sender(Int)` into `Sender(String)` or a
  `Receiver(Int)` into `Receiver(String)`;
- the erase/unerase boundary can keep relying on RFC-0055's endpoint-pairing
  invariant without adding runtime tags to every message.

This deliberately uses RFC-0065 `sealed type`, not `capability`: channel
endpoints are ordinary data handles, not host-authority roots. Sealed type
construction is enough to prevent typed endpoint forgery while preserving
existing endpoint passing and pattern-inspection behavior.

## Acceptance

- The SEC-046 PoC no longer links: endpoint reconstruction from a recovered raw
  id is rejected as sealed construction.
- RFC-0055's pairing invariant is enforced by code, not only by convention in
  `std/chan`.
- `src/example_tests.rs::chan_endpoints_seal_raw_channel_id_construction`
  covers both `Sender` and `Receiver` reconstruction attempts.
- The fix composes with the channel/task dedup: `std/task` owns the executor;
  `std/chan` owns the typed endpoint brands and channel-facing operations.
