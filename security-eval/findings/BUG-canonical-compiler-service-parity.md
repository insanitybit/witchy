# Canonical compiler-service argument and result-size parity

The bounded compiler-service lowerer originally formed an earlier input place
without retaining its reference before evaluating later arguments. A later var
writeback could retire that input before the host call. The shared lowerer now
retains each input at its evaluation point. Documentation's direct and
Result-producing source probes exercise this rule and confirm exact three-leg
output with the original heading and the later mutation's observable effects.

The shared oracle also rejected result sizes above i32::MAX even though the
existing native ABI returns the complete unsigned wasm32 length. It now uses a
checked u32 conversion. Allocation-free tests distinguish 0x7fffffff,
0x80000000, u32::MAX, and overflow. Output capacity is validated before provider
invocation; no short-buffer prefix or pending global buffer is published.

Evidence: 70/70 focused tests, nextest
9a183604-77ee-41e0-b0ea-0047bc0abfee. Independent review confirmed the result-size
finding's disposition. No memory rule or admission requirement was weakened.
