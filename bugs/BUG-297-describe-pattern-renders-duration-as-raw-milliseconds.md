# BUG-297: describe_pattern rendered Duration patterns as raw milliseconds

Severity: LOW
Status: FIXED
Verified: 2026-07-08 FIXED on `worktree-wt-54178-1783511412`
Component: crates/witchy-types/src/typeck.rs, RFC-0052 patterns, diagnostics

## Problem

Duration literal patterns were parsed correctly, but diagnostics described the
normalized millisecond payload instead of a source-level duration spelling.
Examples:

- `let 1s = d` was reported as ``let 1000ms = ...``.
- An unreachable duplicate `1s` match arm was reported as ``1000ms``.

That contradicted RFC-0052's diagnostic direction: pattern errors should not
name synthetic tokens the user never wrote when a clear source-level rendering is
available.

## Fix

`describe_pattern` now renders `Pattern::Duration` through the same compact,
human duration style exposed by `duration.human`: `1s`, `1m30s`, `500ms`, and a
single leading sign for negative durations.

## Regression

- `typeck_tests::duration_pattern_diagnostics_use_human_units`
- `typeck_tests::duplicate_duration_match_arm_is_unreachable`
