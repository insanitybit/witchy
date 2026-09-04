# BUG: canonical literal string.chars failed in tail position

Canonical lowering recognized literal `string.chars` only through the ordinary
value-expression path. A final expression was instead treated as a direct user
call and failed with `MissingBinding("string.chars")`. After tail dispatch was
corrected, static-data planning still omitted the per-Unicode-scalar payloads
synthesized by the intrinsic and failed with `unregistered string payload`.

Tail lowering now routes the intrinsic through the same checked UTF-8 literal
implementation as ordinary value expressions. Static-data planning expands
only literal `string.chars` calls into their Unicode scalar byte payloads. Each
resulting pointer-list element remains an independently owned string copy;
dynamic UTF-8 decoding remains fail-closed.
