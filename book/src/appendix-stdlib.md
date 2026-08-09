# Appendix: The Standard Library

A data type's operations are
**methods** - `xs.map(f)`, `s.trim()`, `d.insert(k, v)` - and every public
method also has a module-qualified alias (`list.map(xs, f)`). Bring a
non-prelude module in with `import name`; its constructors and other
receiver-less helpers are called module-qualified (`iter.range(...)`,
`json.stringify(x)`). A module's
*types and their constructors* are module-scoped the same way - after
`import json` you write `json.JsonInt(1)`. Name a type explicitly with
`from json import Json` to use it and its variant constructors unqualified
(`JsonInt(1)`).

Eight modules form **the prelude** and never need an import line: `list`,
`string`, `dict`, `math`, `option`, `result`, `policy`, and `show`. Every
program can write `xs.push(...)`, `dict.new()`, or `show.render(...)` directly;
an explicit prelude import is accepted but redundant. This appendix is a map; the full,
function-by-function reference - generated from the library sources, so it's
always current - is
[spec/stdlib.md](https://github.com/insanitybit/witchy/blob/master/spec/stdlib.md).

## Collections and data

| Module | What it gives you |
|---|---|
| `list` *(prelude)* | `map`, `filter`, `fold`, `zip`, `sort`, … |
| `dict` *(prelude)* | map operations over `Dict(k, v)` |
| `set` | the `Set(a)` type - distinct values, `union`/`intersection`/`difference`, `for x in set` iteration, and `let s: Set(Int) = iter.collect(it)` (its `FromIterator` is a conditional impl `where a: Eq`). Render a set with `"${s}"` interpolation or `show.say(console, s)` - both work identically on both backends and honor the elements' `Show`. |
| `string` *(prelude)* | `split`, `lines`, `join`, `trim`, case, search, … |
| `path` | path-*string* manipulation (join, normalize, base/dir/ext) - pure; the `Dir`-using half lives in `fs` |
| `bytes` | the `Bytes` type - `from_string`/`to_string_lossy`, `from_list`, `length`, slicing; the binary counterpart to `String` |
| `iter` | lazy iterator combinators (`take`, `collect`, …) |
| `func` | function combinators - `identity`, `compose`, `flip`, `constant`, `on_key`, `first`/`second` |
| `option` / `result` *(prelude)* | helpers for `Option` / `Result` |

Ordering-aware functions (`list.sort_by`, `list.min_by`, …) take a *less-than
predicate* `fn(a, a) -> Bool`, not a three-way compare. Hash functions
(`crypto.sha256`, …) return lowercase hex strings.

Can't find a name? `witchy which <fragment>` searches the whole library -
`witchy which pad` lists both pads with their signatures, and `witchy which
to_ms` finds `duration.to_milliseconds`.

## Formats

| Module | What it gives you |
|---|---|
| `json` | parse and encode JSON - `json.decode(s)` returns `Result(Json, DecodeError)` (the parsed `Json` sum type, or a structured decode error; `json.decode_error_message(e)` renders it), so thread it with `?`; `json.stringify(x)` / `json.from_value(x)` encode *any* value reflectively (give your own types `derive(Reflect)`); `derive(Deserialize)` generates `from_json` to parse a record back with `json.DeserializeError`. There's no `derive(Json)`. |
| `toml` | TOML parsing |
| `url` | URL parsing |
| `encoding` | hex and base64 |
| `regex` | matching, replacement |

## Numbers, time, and traits

| Module | What it gives you |
|---|---|
| `math` *(prelude)* | `sqrt`, `abs`, `min`/`max`, integer `pow`/`isqrt`/`factorial`, `to_int`/`to_float`, … (no trig) |
| `prng` | seeded pseudo-random numbers (deterministic, no capability) |
| `rand` | cryptographically-secure randomness - needs the `Rand` capability |
| `time` / `duration` | civil UTC date-times: `parse_iso8601`, `iso8601`, strftime-style `format`, validated `civil(...)`; `Duration` helpers |
| `semver` | version parsing and comparison |
| `cmp` / `show` *(show is prelude)* | the comparison hierarchy (`PartialEq`/`Eq`/`PartialOrd`/`Ord`, backing `== != < > <= >=`) and display trait; scalar comparison helpers live in `cmp`, collection algorithms live in `list` |
| `convert` / `error` | the `From`/`Into` conversion traits (`(5).into()`, `T.from(x)`) and the `Error` trait bound |
| `borrow` | `.owned()` materialization for a borrowed `View` (and identity on an ordinary owned value); see the performance appendix |
| `reflect` / `meta` | runtime reflection (`reflect(x)` → the `Mirror` tree, `derive(Reflect)`) and compile-time type introspection - see the [Reflection](tour-reflection.md) and [comptime](tour-comptime.md) chapters |
| `ascii` | ASCII classification |

## Concurrency

| Module | What it gives you |
|---|---|
| `chan` | typed channels and the structured-concurrency ladder: `chan.scope` (a nursery that joins or cancels its children on exit - prefer it over a bare `spawn`), `chan.gather`, `chan.par_map` / `chan.par_reduce`, `chan.race`, `chan.select`, and `chan.cancel`. Each channel carries its own message type. |
| `task` | the core task combinators `chan` builds on - `spawn`, `join`, `cancel` - over a pure-witchy deterministic executor |
| `future` | `Future(a)` and the `await` surface |

`vm` runs capture-free work across isolated worker cores (`vm.par_map`); its
`with_dir` / `serve` entry points thread a `Dir` / `Net` through to the workers.
See the [Multi-Core](tour-vm.md) chapter.

## Capability-gated modules

These do real I/O, so their functions take capabilities:

| Module | Capability |
|---|---|
| `fs` | `Dir` |
| `http` / `server` | `Net` |
| `exec` | `Exec` - spawn a native subprocess (`exec.run`); the sharpest authority |
| `crypto` | hashing, verification; signing needs a `Secret` |
| `secretstore` | `SecretStore` - fetch a named host secret (`get`/`require`); reveal with `crypto` |
| `show` | the `Show` trait (with blanket impls for the built-in containers) is pure; `say` (the Show-accepting `print`) takes a `Console`. `"${x}"` interpolation renders through `Show` too |

## Authentication and web identity

Pure-witchy implementations of the standard web-auth protocols, built over
`crypto` / `http` / `json` - the machinery behind the coven registry's trusted
publishing and social login.

| Module | What it gives you |
|---|---|
| `jwt` | verify a compact JWS / JWT (the OIDC identity-token shape), RS256 over `crypto` |
| `oauth` | the OAuth 2.0 Authorization Code flow - "Log in with GitHub / Google" |
| `webauthn` | server-side verification of a WebAuthn assertion (passkey login) |

## Testing

| Module | What it gives you |
|---|---|
| `testing` | the assertions behind `witchy test` (`assert`, `assert_eq`, `assert_value_eq`, …) - see the [Testing](testing.md) chapter |

## Build-time intrinsics

A couple of modules expose witchy's own toolchain to witchy programs - `compiler`
(footprint analysis: this is how the self-hosted package manager audits runes)
and `crypto`'s hashing. These power the tooling you read about in the packages
chapter.

The `rights` module compares rendered capability names with rights-aware
coverage rules (`Dir[Read]`, `Net[Connect, Tcp]`); it's used by supply-chain
tooling. The preluded `policy` module supplies the sealed policy values built by
`Net.tcp`, `Net.cidr`, `Net.private`, `Dir.ext`, and related type methods before
they're passed to `only` or `deny`.

## A reminder about portability

The standard library follows the same parity rule as the language - including rendering the built-in
compound values (lists, tuples, dicts, records, enums, and sets) with `${...}`
interpolation, which is identical on both. Interpolation now also honors a
custom `Show`: `"${x}"` renders through a hand-written `impl Show` (and a
`Duration` in its human form, `1m30s`) exactly as `say` does - see the *Show*
appendix. Case conversion and trimming are deliberately ASCII-scoped on both
backends; other UTF-8 operations, such as character counting and reversal, keep
their documented character semantics. Native services such as cryptographic
verification and networking use host imports in a native run and are unavailable
when a browser host doesn't provide them. An unavailable operation fails loudly
at compilation or instantiation; it never changes meaning silently.
