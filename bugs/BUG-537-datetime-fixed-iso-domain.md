# BUG-537: DateTime constructors allowed years outside the fixed ISO domain

Severity: MED
Status: FIXED
Fixed: 2026-07-07
Component: std/time, DateTime formatting/parsing contract

## Problem

`time.iso8601` and `time.parse_iso8601` expose a fixed four-digit CE year
contract, but the public `DateTime` constructors did not consistently enforce
that same domain. Code could construct dates before year 1 or after year 9999,
then later hit behavior that could not be represented by the documented ISO
format.

## Fix

`time.civil` now rejects years outside `1..9999`, and `time.from_unix` fails if
the computed civil year falls outside that same range. The stdlib docs now state
that `from_unix` is limited to the fixed ISO domain.

Regression coverage:

- `example_tests::datetime_rejects_years_outside_fixed_iso_domain_on_both_backends`
- `example_tests::stdlib_docs_are_current`
