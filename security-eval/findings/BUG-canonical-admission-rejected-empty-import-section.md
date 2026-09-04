# Canonical admission rejected its own empty import section

## Finding

The canonical encoder and its independently constructed metadata prefix both emit an import
section even when the checked module has no imports. The decoded-byte matcher instead omitted the
section from its expected order when the logical import list was empty. Valid host-free artifacts
therefore failed closed with an undifferentiated `PlanMismatch`.

## Resolution

The matcher now states the encoder's exact fixed-section policy, including the empty import
section. Admission mismatch errors identify the failing stage, and section-order mismatches report
both vectors. This changes no accepted byte shape; it makes the independent matcher agree with the
already authenticated encoder and metadata plan.

## Evidence

- `checked_utf8_string_literal_reaches_admitted_static_data_pointer`
