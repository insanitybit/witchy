# BUG-474: `cmp` list equality helpers duplicate `list` and keep stale contracts

Status: FIXED
Severity: MED
Fixed: 2026-07-07 (`fix/delete-cmp-list-duplicates`)

## Resolution

This duplicate of BUG-282 is resolved by the same deletion: the `cmp` list
equality helper quartet is gone, `list` is the canonical collection namespace,
and the stale sentinel-returning `cmp.index_of` contract is no longer public.

Validation: see BUG-282.
