# BUG-381: std rights flattened Net axes and overblocked coverage

Status: FIXED
Severity: MEDIUM
Verified: 2026-07-09
Fixed: 2026-07-09 (`fix/bug381-rights-net-axes`)

## Summary

`std/rights.covers` used a flat marker subset for every capability kind. That
was correct for single-axis rights like `Dir[Read, Write]`, but wrong for
`Net`, whose markers span two independent axes:

- verbs: `Connect`, `Listen`
- transports: `Tcp`, `Udp`, `Uds`

The compiler footprint model treats an omitted `Net` axis as "all values on
that axis". For example, `Net[Connect]` means every Connect transport, so it
covers `Net[Connect, Tcp]`. The std helper rejected that relation because
`Tcp` was not literally present in the declared marker list.

This made package/Coven coverage helpers stricter than the compiler model and
could reject a manifest that accurately admitted the code's capability
footprint.

## Resolution

`std/rights.covers` now handles `Net` with axis-aware coverage:

- omitted verbs expand to `Connect` and `Listen`
- omitted transports expand to `Tcp`, `Udp`, and `Uds`
- both axes are checked independently
- unknown `Net` markers deliberately keep the old flat subset path until
  BUG-154 rejects malformed markers at parse time

Non-`Net` capabilities continue using flat subset semantics.

## Verification

- `CARGO_TARGET_DIR=target-codex-bug381-rights cargo test rights_net_axis_coverage_agrees_on_both_backends -- --nocapture`
- `CARGO_TARGET_DIR=target-codex-bug381-rights cargo test witchy_pm_check_accepts_net_axis_omission -- --nocapture`
- `CARGO_TARGET_DIR=target-codex-bug381-rights cargo test pm_ -- --nocapture`
- `CARGO_TARGET_DIR=target-codex-bug381-rights cargo run --quiet -- fmt --check std/rights.witchy`

The regression test covers both `link_run` and compiled WASM for the Net-axis
truth table, and the e2e test pins `witchy pm check` accepting
`Net[Connect]` as coverage for source demanding `Net[Connect, Tcp]`.
