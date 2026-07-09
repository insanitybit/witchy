# BUG-464: server accessors use empty-string sentinels

Severity: MED
Status: FIXED
Verified: 2026-07-09 REGRESSION on master c02031b3
Component: `std/server`, request accessors, absence semantics

## Problem

The primary request accessors in `std/server` used empty-string sentinel values
for absent path parameters, query parameters, and form fields. That made a
present empty value indistinguishable from an absent value, which is exactly the
case web handlers often need to validate at trust boundaries.

## Resolution

The primary accessors now return `Option(String)`:

- `server.param`
- `server.query`
- `server.form_field`

Callers that intentionally want defaulting use the explicit `_or` variants:

- `server.param_or`
- `server.query_or`
- `server.form_field_or`

Regression coverage:

- `example_tests::server_accessors_return_option_for_absence_on_both_backends`

