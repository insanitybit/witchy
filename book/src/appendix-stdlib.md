# Appendix: The Standard Library

witchy ships ~40 standard-library modules. Bring one in with `import name` and
call its functions module-qualified (`list.map`, `list.join`). A module's
*types and their constructors* are module-scoped the same way — after
`import json` you write `json.JsonInt(1)`. Name a type explicitly with
`from json import Json` to use it and its variant constructors unqualified
(`JsonInt(1)`).

Six modules form **the prelude** and never need an import line: `list`,
`string`, `dict`, `math`, `option`, and `result`. Every program can write
`list.push(...)` or `dict.new()` directly; `import list` is not wrong, just
redundant. This appendix is a map; the full,
function-by-function reference — generated from the library sources, so it's
always current — is
[spec/stdlib.md](https://github.com/insanitybit/witchy/blob/master/spec/stdlib.md).

## Collections and data

| Module | What it gives you |
|---|---|
| `list` *(prelude)* | `map`, `filter`, `fold`, `zip`, `sort`, … |
| `dict` *(prelude)* | map operations over `Dict(k, v)` |
| `set` | the `Set(a)` type — distinct values, `union`/`intersection`/`difference`, `for x in set` iteration, and `let s: Set(Int) = iter.collect(it)` (its `FromIterator` is a conditional impl `where a: Eq`). Render a set with `"${s}"` interpolation or `show.say(console, s)` — both work identically on both backends and honor the elements' `Show`. |
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
| `json` | parse and encode JSON — `json.decode(s)` returns `Result(Json, DecodeError)` (the parsed `Json` sum type, or a structured decode error; `json.decode_error_message(e)` renders it), so thread it with `?`; `json.stringify(x)` / `json.from_value(x)` encode *any* value reflectively (give your own types `derive(Reflect)`); `derive(Deserialize)` generates `from_json` to parse a record back. There is no `derive(Json)`. |
| `toml` | TOML parsing |
| `url` | URL parsing |
| `encoding` | hex and base64 |
| `regex` | matching, replacement |

## Numbers, time, and traits

| Module | What it gives you |
|---|---|
| `math` *(prelude)* | `sqrt`, `abs`, `min`/`max`, integer `pow`/`isqrt`/`factorial`, `to_int`/`to_float`, … (no trig) |
| `prng` | seeded pseudo-random numbers (deterministic, no capability) |
| `rand` | cryptographically-secure randomness — needs the `Rand` capability |
| `time` / `duration` | civil UTC date-times: `parse_iso8601`, `iso8601`, strftime-style `format`, validated `civil(...)`; `Duration` helpers |
| `semver` | version parsing and comparison |
| `cmp` / `show` | the comparison hierarchy (`PartialEq`/`Eq`/`PartialOrd`/`Ord`, backing `== != < > <= >=`) and display trait, plus generic algorithms (`list.sort`, `cmp.max_of`, …) |
| `ascii` | ASCII classification |

## Concurrency

| Module | What it gives you |
|---|---|
| `chan` | typed channels and the structured-concurrency ladder: `chan.scope` (a nursery that joins or cancels its children on exit — prefer it over a bare `spawn`), `chan.gather`, `chan.par_map` / `chan.par_reduce`, `chan.race`, `chan.select`, and `chan.cancel`. Each channel carries its own message type. |
| `task` | the core task combinators `chan` builds on — `spawn`, `join`, `cancel` — over a pure-witchy deterministic executor |
| `future` | `Future(a)` and the `await` surface |

`vm` (multi-core) is capability-gated — see below.

## Capability-gated modules

These do real I/O, so their functions take capabilities:

| Module | Capability |
|---|---|
| `fs` | `Dir` |
| `http` / `server` | `Net` |
| `crypto` | hashing, verification; signing needs a `Secret` |
| `show` | the `Show` trait (with blanket impls for the built-in containers) is pure; `say` (the Show-accepting `print`) takes a `Console`. `"${x}"` interpolation renders through `Show` too |

## Build-time intrinsics

A couple of modules expose witchy's own toolchain to witchy programs — `compiler`
(footprint analysis: this is how the self-hosted package manager audits runes)
and `crypto`'s hashing. These power the tooling you read about in the packages
chapter.

## A reminder about portability

Most of the library works on every backend — including rendering the built-in
compound values (lists, tuples, dicts, records, enums, and sets) with `${...}`
interpolation, which is identical on both. Interpolation now also honors a
custom `Show`: `"${x}"` renders through a hand-written `impl Show` (and a
`Duration` in its human form, `1m30s`) exactly as `say` does — see the *Show*
appendix. A few operations remain interpreter-only by nature: the Unicode-aware
string operations the WebAssembly backend scopes to ASCII. A quick `witchy
<file>` run confirms a program is sandbox-clean.
