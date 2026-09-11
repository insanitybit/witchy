# Explicit socket close can leave the send half open after peer EOF

Status: corrected in integration commit `7d5702619`; landing pending.

On Darwin, `shutdown(Both)` after reading peer EOF can return `ENOTCONN`
without shutting the still-live send half. The existing infallible close
implementation discarded that error. Successful output and eventual VM or
interpreter teardown concealed the missing explicit close effect.

The reproducer keeps the socket alive after reading peer EOF, sends an
acknowledgement, and closes it. The peer must observe EOF before either
execution engine tears down. A second socket provides that completion barrier
in the full source/Wasm/oracle regression.

Regression coverage is in `tests/misc/canonical_net_fixture.rs`, module
`half_close`. It also checks repeated close and retained socket aliases.
The fix independently shuts down the write and read halves. TLS close-notify
and flushing still precede transport shutdown. The Unit/idempotence contract,
authentication, buffered read-ahead, and staged receive output are unchanged.

The combined integration selector recorded 19 passing tests in
`/tmp/witchy-task-net-integrated-focus.log`, including these regressions,
existing Net conformance, and the full source-program fixture. This is focused
behavioral evidence, not a compiler-wide memory-safety claim.
