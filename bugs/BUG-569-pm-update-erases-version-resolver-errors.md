# BUG-569: PM update erased typed version resolver errors

Status: FIXED
Severity: HIGH
Component: `projects/pm`, update trust metadata, RFC-0054

## Summary

`resolve_version` distinguishes registry fetch failure, malformed response,
bad requirements, cooldown, and no matching release with
`VersionResolveError`. The `pm add` path rendered those errors, but
`update_one` mapped every `Err` to `(changed = 0, blocked = false, failed =
false)`.

A malformed or unavailable `/coven/versions` response therefore looked like a
successful update with no newer release. The outer update path could continue
to regenerate `witchy.lock` and repin registry metadata despite never having a
trustworthy version decision.

## Resolution

`update_one` keeps only `VersionCooling` and `VersionNoMatch` as benign no-op
outcomes. Registry fetch failure, malformed response, and a bad manifest
requirement now print the typed resolver error and mark the update failed. The
existing failed-update path preserves `witchy.lock` and its pinned root key.

End-to-end coverage serves a wrong-shaped versions document to `pm update` and
checks that the typed malformed-response diagnostic survives, the command
fails, and the original lockfile remains byte-identical.
