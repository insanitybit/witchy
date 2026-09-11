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
Follow-up reproduction found a second genuine bug in `4bc919cfa`: the
source `print(recv_bytes(1)); close(); print(recv_bytes(1))` against a
peer writing `ab` prints `["a", "b"]` natively but `["a", ""]` in the
oracle. The `recv_all()` variant shows the same mismatch. Native's
buffered reader can retain `b` after shutdown; local close is not peer EOF.

Wire event boundaries do not identify which bytes native prefetched.
Until the fixture can describe that residency independently, post-close
line/all/nonzero-byte receives explicitly return
`UnsupportedOperation("Net fixture post-close receive")` without consuming
events or staging fabricated output. Staged fills and zero-byte receives
remain supported. This is an explicit post-close oracle coverage gap,
not a claim of complete native/oracle parity or a native semantics change.

Missing/wrong closes, sends, other sockets' events, and EOF/timeout/error
events are barriers. Refusal changes neither events, partial offsets,
staging, nor socket state. Events after the matching close remain pending.

Focused coverage: oracle close success/refusal matrices and post-close
bounded-fill retries; both canonical launch APIs versus a two-byte
loopback peer; native close-before-fill with exact byte/sentinel checks.
Post-close byte/all/line probes require native output `["a", "b"]` and
explicit refusal through both oracle launch APIs.

## Explicit buffered profile

Opt-in `Buffered` immediately follows authenticated Connect/Accept. `Arrive`
only appends transport-ready bytes; `StreamRead(Data(n))` moves an explicit
prefix into read-ahead, with `0 < n <= 8192` and no more than the requested
count or available bytes. Native conformance additionally enforces the actual
underlying `Read` destination size. No event chunk implies a kernel read.
`BufferedClose(keep_transport_prefix)` preserves all read-ahead and staging,
retains only the specified transport prefix, and discards its tail. Reads
after close consume read-ahead or explicit stream completions, never guessed
EOF. Old wire-only fixtures keep the c89 refusal.

Protocol mismatches roll back the entire oracle receive. Real IO errors and
raw-byte-cap failures consume completed reads but stage no fabricated output.
The independent native test transport is also supplied to a feature-gated
driver of the existing source-interpreter Net builtins. No production dial
hook, import, checker operation, or shared oracle/native receive engine exists.
No checker, lowering, native runtime, or source language semantics changed.
