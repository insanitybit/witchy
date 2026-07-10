# BUG-161: LSP hover renders stdlib API with invalid or missing spellings

Severity: MED
Status: FIXED
Verified: 2026-07-09 - `string.repeat`, `Net.tcp`, and `Dir.ext` hover with callable spellings
Component: LSP hover, std API discovery, RFC-0057 associated constructors
Found: 2026-07-05

## Summary

LSP hover historically treated all dotted calls as module functions. Ordinary
stdlib functions were once rendered as malformed text such as
`string.pub fn repeat(...)`; after that prefix bug was fixed, associated policy
constructors still fell through to their implementation module and appeared as
the uncallable `policy.tcp(...)`, or had no hover.

## Historical Reproduction

```witchy
import string

fn main(console: Console):
    let net_policy = Net.tcp("127.0.0.1", 8080)
    let dir_policy = Dir.ext(".log")
    console.print(string.repeat("x", 2))
```

The program checks and runs, but editor hover did not reflect its valid source
spelling.

## Resolution

`witchy-syntax::doc` now exposes a narrow AST-backed lookup for public,
self-less inherent functions. It reuses the same signature renderer as
generated API documentation and returns the owning type, parameters, return
type, async/generator qualifier, and adjacent docs as one structured result.

LSP hover classifies an uppercase dotted head as a type owner, searches the
current and visible modules through that API, and does not fall back to an
unrelated free function. Lowercase receiver methods and ordinary module
functions retain their existing paths.

## Acceptance

- `string.repeat` remains `pub fn string.repeat(...)`, never `string.pub fn`.
- `Net.tcp` renders `Net.tcp(host: String, port: Int) -> NetPolicy`.
- `Dir.ext` renders `Dir.ext(suffix: String) -> DirPolicy`.
- AST-renderer and end-to-end LSP regressions cover ownership and spelling.
