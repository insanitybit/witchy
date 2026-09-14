# Fetch chunk-size arithmetic can panic on a malformed response

Status: repaired; regression passed in the 119/119 focused run
`723450bf-309a-45ac-996f-265ef99b1899`.

The existing HTTP provider's `decode_chunked` added two to an untrusted
`usize` chunk length before checking the available body. A size line of
`ffffffffffffffff\r\n` on a 64-bit host overflows that addition. This can
panic instead of returning the documented malformed-response value. It is
a host robustness and parity defect, not evidence of memory corruption or
an authority bypass.

Canonical Fetch review exposed the discrepancy: its independent oracle uses
checked arithmetic, while the existing native provider did not. The repair
checks the addition and preserves the `truncated chunk body` error. The test
`fetch_chunk_size_overflow_is_a_malformed_response_not_a_panic` covers the
three largest representable sizes and a valid chunked body.

The provider's origin policy, TLS, Net floor, response-size limit and error
classification are unchanged. This finding does not establish compiler-wide
memory safety.
