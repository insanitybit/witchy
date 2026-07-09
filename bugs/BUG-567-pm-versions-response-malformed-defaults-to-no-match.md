# BUG-567: Malformed PM versions responses defaulted to no match

Status: FIXED
Severity: MEDIUM
Component: `projects/pm`, RFC-0054 trust boundaries

## Summary

The package manager decoded `/coven/versions` through a fail-soft helper that
returned an empty list for invalid JSON, a missing or non-array `records`
field, malformed records, and noncanonical version coordinates. Install and
update resolution then reported the same `VersionNoMatch` used for a valid
registry with no satisfying release.

This was fail-closed for installation, but it erased a trust-boundary failure:
registry corruption looked like ordinary package availability and could not be
diagnosed or matched by callers.

## Resolution

Version decisions now parse the response once into
`Result(List((Version, Int)), VersionResolveError)`. Invalid JSON and record
shape, unknown states, noncanonical versions, and invalid release timestamps
produce `VersionMalformedResponse`; cooldown and semver selection consume only
the validated values. End-to-end coverage verifies that a wrong-shaped
`records` field fails before vendoring, and the existing noncanonical-coordinate
test now expects a malformed-response diagnostic.

`pm list` retains its separate fail-soft projection intentionally: it is a
display-only view and cannot select or install a coordinate.
