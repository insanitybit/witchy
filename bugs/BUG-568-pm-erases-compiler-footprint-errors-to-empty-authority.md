# BUG-568: PM erased compiler footprint errors to empty authority

Status: FIXED
Severity: HIGH
Component: `projects/pm`, capability gates, RFC-0054

## Summary

`compiler.footprint(source)` returns a JSON error document when source cannot be
parsed, linked, type-checked, or safely expanded. PM helpers decoded that report
with `json.get_strings(...).unwrap_or([])`-style behavior. A compiler error,
malformed JSON, a missing footprint array, or a non-string element therefore
became the same empty list as a valid capability-free program.

This crossed an authority boundary. `pm check` could approve uninspectable
source, `pm guard` could approve an uninspectable update because missing
`widened` defaulted to false, `pm lock` could pin an uninspectable path
dependency with an empty `runtime_footprint`, and build/generated/dependency
gates could compare against an under-decoded empty demand set. BUG-179 fixed
the compiler producer so it reports invalid source; PM then erased that failure
at consumption.

## Resolution

PM now parses required `total` and `build` arrays through a local typed
`FootprintError`. Compiler rejection, malformed report JSON/shape, and
non-string capability entries remain errors. The `Result` is threaded through
build grants, generated-source auditing, source checks/diffs, path lock
generation, local dependency gates, contributor diagnostics, and post-vendor
closure gates. Presentation commands render the same error rather than printing
no authority.

End-to-end coverage uses type-invalid source and verifies `pm check`, `pm guard`,
and `pm lock` fail with the compiler error and no lockfile is written.
