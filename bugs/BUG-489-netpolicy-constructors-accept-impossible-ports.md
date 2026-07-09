# BUG-489: NetPolicy constructors accept impossible port numbers

Severity: LOW
Status: FIXED
Verified: 2026-07-09 FIXED on master 3ca29da
Component: `std/policy`, network capability refinement, generated stdlib docs, `witchy-caps`
Discovered: 2026-07-06

## Summary

`std/policy` presents `Net.tcp(host, port)` and `Net.cidr(block, port)` as the
typed constructors for network refinement policies, but those constructors accept
any `Int` port. Negative ports and values above `65535` are concatenated into the
raw `host:port` policy grammar instead of failing at the API boundary.

This does not grant extra network authority. It does make the blessed policy
surface less crisp: ordinary code can build impossible policy values without
using the raw `NetPolicy(pattern)` escape hatch, and the runtime then treats them
as ordinary patterns.

## Code Evidence

- `std/policy.witchy:41-44` implements `Net.tcp(host, port)` as
  `NetPolicy(checked_component(host) + ":" + "${port}")`.
- `std/policy.witchy:50-52` implements `Net.cidr(block, port)` the same way.
- `std/policy.witchy:25-28` only rejects newline/carriage-return delimiter
  injection in string components; it does not validate the numeric `port`.
- `crates/witchy-caps/src/capabilities.rs:119-145` treats policy entries as raw
  `host:port` strings. A pattern like `example.com:-1` or
  `10.0.0.0/8:70000` is just another pattern.
- `crates/witchy-caps/src/capabilities.rs:225-234` lets `net.only` keep any
  pattern admitted by the current allowlist. For example, a broad `host:*` grant
  can admit `host:-1`, producing a narrowed `Net` that cannot reach a real port.
- `crates/witchy-runtime/src/runtime.rs:2109-2115` and
  `crates/witchy-interp/src/interpreter.rs:1901-1910` implement `net.deny` by
  appending the raw pattern with a `!` prefix, so `net.deny(Net.tcp(host, -1))`
  silently subtracts nothing useful.

## Impact

For a public release, capability refinements should have a clear boundary:
constructors either create a meaningful policy value or fail loudly. Witchy
already applies that rule in neighboring APIs: `std/url.parse` rejects invalid
ports, and `std/policy` now rejects delimiter injection through blessed
constructors.

Invalid ports are most likely user error. Silently constructing a dead `Net` via
`only` or a no-op denial via `deny` makes policy code harder to audit and debug.

## Why This Is Distinct

- BUG-359 is fixed for newline/carriage-return injection through blessed
  `Net.*` constructors.
- BUG-484 tracks direct raw `NetPolicy(pattern)` construction exposing the
  internal policy grammar.
- BUG-351 tracks URL IPv6 parsing not matching Net policy support.

This bug is specifically about missing numeric validation in the documented
`Net.tcp` and `Net.cidr` constructors, even when callers never touch
`NetPolicy(...)` directly.

## Expected Behavior

`Net.tcp(host, port)` and `Net.cidr(block, port)` should reject ports outside
`0..=65535` with a clear contract failure, or return a fallible policy type if
the API chooses to make malformed policy construction recoverable. The
constructor docs should state the valid range.

## Acceptance Criteria

- `Net.tcp("example.com", -1)` and `Net.tcp("example.com", 70000)` fail loudly
  before reaching `net.only` / `net.deny`.
- `Net.cidr("10.0.0.0/8", -1)` and `Net.cidr("10.0.0.0/8", 70000)` fail the
  same way.
- Valid edge ports `0` and `65535` keep their current behavior, or the docs state
  a narrower deliberate range.
- Generated stdlib docs describe the accepted port range for policy
  constructors.

## Fixed

Current `std/policy.witchy` routes `Net.tcp(host, port)` and
`Net.cidr(block, port)` through `checked_port`, which fails loudly unless
`port` is in `0..65535`. The generated stdlib docs carry that range in the
constructor comments.

Regression:

- `net_policy_constructors_reject_out_of_range_ports_on_both_backends` covers
  `Net.tcp`/`Net.cidr` for `-1`, `70000`, and edge ports `0`/`65535` on both the
  interpreter and compiled WASM backend.
