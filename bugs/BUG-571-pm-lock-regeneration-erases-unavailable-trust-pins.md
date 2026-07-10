# BUG-571: PM lock regeneration erased unavailable trust pins

Status: FIXED
Severity: HIGH
Component: `projects/pm`, TUF/TOFU lock metadata, RFC-0054

## Summary

`snapshot_pin` returned `Ok("")` for every non-200 snapshot response, and
`rootpub_pin` returned `""` when the registry root key was unavailable. Add and
update concatenated those empty values into a newly generated `witchy.lock`.

A transient or hostile metadata failure could therefore make an otherwise
successful no-op update rewrite the lock without its existing TUF rollback
floor or TOFU root-key anchor. The next operation would have less trust state
than the previous one, even though no explicit trust reset was requested.

## Resolution

One typed `lock_trust_pins` boundary now obtains both lines before lock
serialization. `TufError` distinguishes unavailable snapshot and root-key
responses (including the HTTP status); either failure stops add/update before
the lock is written. There is no empty-pin representation.

End-to-end coverage starts from a real signed registry and pinned lock, then
proxies valid version/TUF metadata while failing first the snapshot fetch and
then the final root-key fetch. Both updates fail with the precise typed
diagnostic and leave the original lockfile byte-identical.
