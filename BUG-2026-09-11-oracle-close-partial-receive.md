# Oracle close falsely refused partially consumed incoming data

Genuine behavior bug in `ced5fc663`: after `Receive("ab")`, receiving one
byte left that event at the transcript front. An immediately scripted
`Close` then incorrectly returned `TranscriptMismatch`; native shutdown
succeeds and preserves any already staged receive result for its fill.

The fixture transcript describes wire events, not individual hostcall
results. Close now preflights consecutive incoming `Receive` events for
its authenticated socket followed immediately by its exact `Close`.
Success discards those unread bytes and records each original event once,
including the partially consumed event. Staged output remains available.
The fixture treats discarded ingress as unavailable to later reads; it
does not model native buffered read-ahead after local shutdown.

Missing/wrong closes, sends, other sockets' events, and EOF/timeout/error
events are barriers. Refusal changes neither events, partial offsets,
staging, nor socket state. Events after the matching close remain pending.

Focused coverage: oracle close success/refusal matrices and post-close
bounded-fill retries; both canonical launch APIs versus a two-byte
loopback peer; native close-before-fill with exact byte/sentinel checks.
No checker, lowering, native runtime, or source language semantics changed.
