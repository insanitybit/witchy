# BUG-481: Duration numeric constructors bypass parse overflow policy

Severity: MED
Status: FIXED
Verified: 2026-07-06 CODE on master 7bb3ee7
Component: `std/duration`, generated stdlib docs, numeric helper contracts

## Summary

`std/duration.parse` rejects millisecond totals that exceed the signed 64-bit
backing range, but the public numeric constructors used direct `Int`
multiplication. Calls such as `duration.seconds(9223372036854776)` and
`duration.days(200000000000)` could silently wrap before becoming a `Duration`.

## Resolution

Fixed by making `milliseconds`, `seconds`, `minutes`, `hours`, `days`, `weeks`,
and `from_clock` abort with a function-specific overflow message before scaling
or adding can wrap. `from_clock` checks each component and the signed component
sum. `duration.parse` remains the fallible API for user input.

The generated stdlib reference documents that numeric constructors are
convenience contracts that abort on overflow, and
`duration_numeric_constructors_abort_on_overflow_on_both_backends` covers
overflow and ordinary negative spans on both backends.
