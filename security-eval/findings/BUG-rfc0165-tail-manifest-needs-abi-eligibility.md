# BUG: RFC-0165 proper-tail manifest is syntactic, not ABI-qualified

## Status

Open on `impl/rfc0165-canonical-v1`.

## Finding

`TypedModule::proper_tail_manifest` currently records every call in a syntactic
RFC-0090 tail position. RFC-0090 requires an additional semantic test: the
complete result envelope, `var` write-backs, ownership state, capability and
reference kinds, and loan cleanup must be forwarded without caller-side work.
A final `f(var xs)` is therefore not normally a proper tail edge even though it
is textually final.

The current canonical lowerer does not create an unsafe or incorrect artifact:
its v1 scalar signature path rejects `var` and `own` conventions, and unsupported
tail shapes fail before candidate construction. The defect is latent but
load-bearing because broader callable coverage must not interpret the manifest
as authority in its current form.

## Required correction

Tail-site derivation must classify each syntactic candidate against the exact
checked callable ABI. Only a site whose entire continuation/result envelope is
forwarded unchanged becomes a `ProperTailSite` obligation. A syntactic tail call
whose compatibility cannot be established must remain explicitly classified as
residual-work or unsupported so canonical lowering rejects it rather than emits
an ordinary call or an unjustified `TailCall`.

Focused coverage must include:

- a scalar self-tail call admitted as proper;
- a final `var` call classified non-proper unless its complete write-back
  envelope is forwarded;
- a call requiring a post-call drop or ownership cleanup classified non-proper;
- capability, GC-reference, and loan result-kind mismatches rejected;
- a proper site missing from canonical SafeWIR rejected by the initial checker.
