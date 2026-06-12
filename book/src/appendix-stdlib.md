# Appendix: The Standard Library

witchy ships ~30 standard-library modules. Bring one in with `import name` and
call its functions module-qualified (`list.map`, `string.join`). A module's
*types and their constructors* come in unqualified, though — after `import json`
you write `JsonInt(1)`, not `json.JsonInt(1)`.

Six modules form **the prelude** and never need an import line: `list`,
`string`, `dict`, `math`, `option`, and `result`. Every program can write
`list.push(...)` or `dict.new()` directly; `import list` is not wrong, just
redundant. This appendix is a map; the full,
function-by-function reference — generated from the library sources, so it's
always current — is
[docs/stdlib.md](https://github.com/insanitybit/witchy/blob/master/docs/stdlib.md).

## Collections and data

| Module | What it gives you |
|---|---|
| `list` *(prelude)* | `map`, `filter`, `fold`, `zip`, `sort`, … |
| `dict` *(prelude)* | map operations over `Dict(k, v)` |
| `set` | set operations |
| `string` *(prelude)* | `split`, `lines`, `join`, `trim`, case, search, … |
| `path` | path-*string* manipulation (join, normalize, base/dir/ext) — pure; the `Dir`-using half lives in `fs` |
| `iter` | lazy iterator combinators (`take`, `collect`, …) |
| `option` / `result` *(prelude)* | helpers for `Option` / `Result` |

Ordering-aware functions (`list.sort_by`, `list.min_by`, …) take a *less-than
predicate* `fn(a, a) -> Bool`, not a three-way compare. Hash functions
(`crypto.sha256`, …) return lowercase hex strings.

Can't find a name? `witchy which <fragment>` searches the whole library —
`witchy which pad` lists both pads with their signatures, and `witchy which
to_ms` finds `duration.to_milliseconds`.

## Formats

| Module | What it gives you |
|---|---|
| `json` | parse and encode JSON — `json.decode(s)` returns the `Json` sum type; `derive(Json)` on a record generates `to_json` |
| `csv` | RFC 4180 CSV |
| `toml` | TOML parsing |
| `url` | URL parsing |
| `encoding` | hex and base64 |
| `regex` | matching, replacement |

## Numbers, time, and traits

| Module | What it gives you |
|---|---|
| `math` *(prelude)* | `sqrt`, `pow`, trig, `abs`, … |
| `random` | seeded pseudo-random numbers |
| `time` / `duration` | civil UTC date-times: `parse_iso8601`, `iso8601`, strftime-style `format`, validated `civil(...)`; `Duration` helpers |
| `semver` | version parsing and comparison |
| `eq` / `ord` / `show` | the comparison/ordering/display traits and generic algorithms |
| `ascii` | ASCII classification |

## Capability-gated modules

These do real I/O, so their functions take capabilities:

| Module | Capability |
|---|---|
| `fs` | `Dir` |
| `http` / `server` | `Net` |
| `crypto` | hashing, verification; signing needs a `Secret` |
| `show` | the trait and `show_list` are pure; `say` (the Show-accepting `print`) takes a `Console` |

## Build-time intrinsics

A couple of modules expose witchy's own toolchain to witchy programs — `compiler`
(footprint analysis: this is how the self-hosted package manager audits runes)
and `crypto`'s hashing. These power the tooling you read about in the packages
chapter.

## A reminder about portability

Most of the library works on every backend — including rendering whole compound
values with `${...}` interpolation, which is identical on both. A few
things are interpreter-only by nature (e.g. Unicode-aware operations the
WebAssembly backend scopes to ASCII). The compiler tells you — loudly — if you
use one in code headed for the sandbox, so you never have to guess.
