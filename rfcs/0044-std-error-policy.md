---
rfc: 0044
title: A written error and return-shape policy for std
status: proposed
created: 2026-07-03
predecessors:
  - "0021 (Option || T unwrap — the ergonomic floor that makes Option cheap)"
  - "scratch/consistency-analysis-2026-07-03.md §5 (the audit this enacts)"
tracking:
---

# RFC-0044: A written error and return-shape policy for std

## Summary

std has a real error/return policy — lookup miss → `Option`, invalid input →
`Result`, programmer error → abort, ergonomics via `_or` — but it is written
down nowhere and followed only ~70% of the time. This RFC writes the policy
down as five normative rules (destined for `CONTRIBUTING.md` and a new
`spec/error-policy.md`), decides the contested cases (regex, crypto verify,
toml's fake `Result`), and migrates every violator to its policy shape in one
cut. This RFC changes **shapes**; [RFC-0049](0049-naming-lexicon.md) changes
**names** — where a function gets both (e.g. `index_of`), the two land in the
same cut but each decision lives in its own RFC.

## Motivation

The book disavows sentinels in its second paragraph (`book/src/tour-errors.md:8-9`:
"without inventing a sentinel like `-1` or `null`") while six std functions
return `-1` today (`string.index_of`, `string.last_index_of`, `list.index_of`,
`list.find_index`, `cmp.index_of`, `ascii.to_digit`) — and `std/list.witchy:280`
carries a doc comment *apologizing* for one of them. The same audit
(consistency report §5) found every worse variant of the pattern:

- **`""`/`[]` sentinels**: `string.char_at` returns `""` out of range;
  `server.param`/`query`/`form_field` return `""` on a missing key while
  `request_header` *in the same module* returns `Option`; all four
  `encoding.*decode` functions return `""` on malformed input — and they feed
  the crypto/jwt/webauthn stack, so a corrupted token silently becomes an empty
  payload instead of an error; `fs.parent_dir` returns `""`;
  `json.get_strings`/`toml.get_array` return `[]` for absent-or-wrong-shape.
- **Indistinguishable failure**: `crypto.ed25519_verify` (and the ECDSA/RSA
  twins) return `false` for a malformed key — the caller cannot tell "bad
  signature" from "I passed garbage where a key goes". Its own doc admits the
  conflation ("malformed input or a bad signature yields `false`, never an
  error", `std/crypto.witchy:22-24`).
- **Silent defaults**: `math.to_int(NaN)` → 0, `math.isqrt(-5)` → 0,
  `math.factorial(-5)` → 1, `math.pow(x, -1)` → 1,
  `time.days_in_month(y, 13)` → 31 (the `_ -> 31` arm, `std/time.witchy:71-79`).
  Worst of the bucket: `list.set_at`/`update_at` on an out-of-range index
  **silently no-op** (`std/list.witchy:556-558`) while `list.at` *read* traps —
  write-OOB silent, read-OOB loud is inverted safety and a data-loss factory.
- **Result mimicry**: `toml.decode` returns `Result` but cannot fail — its own
  doc says so ("Always succeeds — malformed lines are skipped — so the result
  is `Ok`; the `Result` shape mirrors `json.decode`", `std/toml.witchy:24-27`).
  `csv.parse` absorbs malformed rows the same way.
- **Shape defector**: `duration.parse` → `Option` where `semver`/`time`/`url`/
  `json` all return `Result` for the identical failure concept.
- **Doc-vs-impl contradiction**: `std/regex.witchy:1-3` promises "a loud
  error, not a silent non-match, on an invalid pattern"; the implementation
  (`crates/witchy-runtime/src/native.rs`, `match_spans`) deliberately returns
  no-matches on an invalid pattern, with a comment defending totality and
  admitting "a latent parity gap too". One of them is lying to users.

Each violation is small; together they mean a user cannot predict what a std
function does on bad input without reading its source. A written policy makes
the next hundred functions consistent by default and makes review mechanical.

## Design

### The five rules (normative; new `spec/error-policy.md`, summarized in CONTRIBUTING.md)

1. **Absence is `Option`. Never `-1`, `""`, or `[]`.** A function that looks
   something up and may not find it returns `Option(T)`. Ergonomics come from
   `_or` variants (`get_or`), the existing `Option || T` unwrap (RFC-0021), and
   the `??` fallback operator ([RFC-0048](0048-fallback-operator.md)) — sequence
   0048 into the same release so the migration never feels like a downgrade.
2. **Invalid input is `Result(T, String)` — and the `Err` must be reachable.**
   Parsing, decoding, and validation of data return `Result`. A `Result` that
   is always `Ok` is banned (it teaches callers to skip the match). Error
   strings are lowercase, echo the offending input in backticks, and name the
   expected form. `std/time` is the house style:
   ``Err("`${t}` is not an ISO 8601 date (expected `YYYY-MM-DD`)")``.
3. **Contract violation aborts — identically and legibly on both backends.** A
   caller breaking a stated precondition (index out of range, negative
   factorial, month 13) is a programmer error: abort, don't absorb. "Legibly"
   is [RFC-0045](0045-compiled-trap-diagnostics.md) — today the compiled abort
   is a bare `unreachable`, which is why silent defaults felt kinder than they
   are.
4. **I/O is a trapping primitive plus a `try_` twin returning `Result`.**
   `http.get`/`http.try_get` is the model. The doc-comment of each half says
   which it is and names its twin.
5. **Nothing silently clamps or defaults unless the name says so.** `get_or`,
   `clamp`, `unwrap_or` may default — the name is the contract. Documented
   *total* behavior is grandfathered where totality is the useful behavior
   (`substring`/`take`/`drop` clamping to bounds), and the doc-comment must
   state it. A default the name doesn't announce (`factorial(-5) == 1`) is a
   rule-3 abort instead.

Error payloads stay `String` in this RFC; a structured error type is
[RFC-0054](0054-structured-errors.md)'s question, and these rules are written
to survive that change (rule 2 becomes `Result(T, E)` with the same voice
rules on `E`'s rendering).

### Decisions on the contested cases

- **regex: the header wins — invalid pattern is a loud error.** The impl
  comment's totality argument protects a hypothetical attacker-supplied
  pattern at the cost of every real user's typo being a silent no-match, and
  it leaves the module's own header false. An invalid pattern is a contract
  violation (rule 3): both backends abort with
  ``runtime error: invalid regex pattern `...`: <engine detail>`` (legible via
  RFC-0045). For genuinely untrusted patterns, add
  `regex.validate(pattern: String) -> Result(Nil, String)` so a program can
  vet before use — rule 4's shape, applied to compilation instead of I/O.
  The RE2-semantics engine already makes *valid* patterns linear-time, so the
  DoS argument reduces to "reject invalid ones loudly", which is this fix.
- **crypto verify: `Result(Bool, String)`.** `Ok(true)` = signature valid,
  `Ok(false)` = well-formed input, wrong signature, `Err` = malformed
  key/signature encoding. The jwt/webauthn/oauth stack updates in the same cut
  (they already return `Result`, so `?` composes). This is a security fix as
  much as a shape fix: today a truncated hex key verifies as `false`, which
  reads as "tampered token" instead of "your key loading is broken".
- **toml/csv honesty**: `toml.decode`'s `Err` becomes reachable (malformed
  line → `Err` naming the line and what was expected, rule-2 voice);
  `csv.parse` (renamed `decode` by RFC-0049) returns
  `Result(List(List(String)), String)` and errors on structurally malformed
  input (e.g. an unterminated quote) instead of absorbing it.

### The migration table (one cut, ordered by user impact)

| Function(s) | Today | New shape | Rule |
|---|---|---|---|
| `string.index_of`, `string.last_index_of`, `list.index_of`, `ascii.to_digit` | `Int` (−1) | `Option(Int)` (`to_digit`: `Option(Int)`) | 1 |
| `list.find_index` | `Int` (−1) | **deleted** — by-predicate search is `position` (name decision in RFC-0049) | 1 |
| `cmp.index_of` | `Int` (−1) | **frozen** — the whole `cmp.member`/`index_of`/`count`/`unique` quadruplet is deleted once [RFC-0046](0046-typed-trait-dispatch.md) lets `list.*` carry `Eq` bounds; do not churn it twice | — |
| `math.to_int(NaN)`, `math.isqrt(neg)`, `math.factorial(neg)`, `math.pow(x, neg)` | 0 / 0 / 1 / 1 | abort with a message naming the bad argument (`to_int`'s out-of-range *saturation* stays, documented — rule-5 grandfather) | 3 |
| `list.set_at`, `list.update_at` (OOB) | silent no-op | trap, matching `list.at` — read and write agree on loud | 3 |
| `duration.parse` | `Option(Duration)` | `Result(Duration, String)` | 2 |
| `toml.decode`, `csv.parse` | always-`Ok` `Result` / total | genuinely fallible `Result` (above) | 2 |
| `server.param`, `server.query`, `server.form_field` | `""` | `Option(String)`, aligning with `request_header` | 1 |
| `encoding.hex_decode`, `base64_decode`, `base64url_decode`, `base64url_to_hex` | `""` on bad input | `Result(String, String)`; jwt/webauthn/oauth callers updated in-cut | 2 |
| `time.days_in_month(y, mo)` (mo ∉ 1..12) | 31 | abort: ``month `13` is out of range (expected 1..12)`` | 3 |
| `string.char_at` | `""` | `Option(String)` | 1 |
| `crypto.*_verify` | `Bool` | `Result(Bool, String)` (above) | 2 |
| `fs.parent_dir` | `""` | `Option(String)` | 1 |
| `json.get_strings`, `toml.get_array` | `[]` | **grandfathered** under rule 5: they are `get_or`-family conveniences whose default-to-empty *is* the useful total behavior; their doc-comments must say "lenient by design" and name the strict alternative (`get` + `as_array`) | 5 |

Every changed function's doc-comment is updated in `std/*.witchy` and
`spec/stdlib.md` regenerated (`witchy doc std/*.witchy > spec/stdlib.md`) —
never hand-edited. Every migrated function gets a differential test on both
backends; the abort rows depend on RFC-0045 for message-level parity but not
for the shape change itself.

## Alternatives

- **Do nothing / document the strata.** Marking functions "old-style" freezes
  the inconsistency and still leaves `set_at`'s silent data loss and the
  crypto conflation in place. Rejected.
- **Deprecation aliases (`index_of` stays, `find_index_opt` added).** Violates
  break-don't-deprecate; produces the exact two-idiom surface the audit
  scored 5.5/10. Rejected.
- **Sentinels are fine, document them (Go's position).** Go's `-1` works
  because Go has no `Option` and one giant ecosystem convention. witchy has
  `Option`, `||`-unwrap, and a book that already promises no sentinels; the
  cost of keeping both idioms is a coin-flip on every lookup. Rejected.
- **`Result` for regex instead of abort** (make every `regex.*` call
  fallible). Punishes the 99% static-pattern case with match-noise to serve
  the untrusted-pattern case, which `regex.validate` serves better. Rejected.

## Drawbacks

- **This is a large breaking cut** — ~20 functions across 12 modules, plus
  every in-repo caller (std internals, `examples/`, `projects/pm`, `coven`,
  book fences). The suite and executed docs are the migration vehicle, but
  it is real churn, and out-of-tree witchy code (there is little, pre-1.0)
  breaks without a compiler hint beyond the type error.
- **`Option`-heavy code is slightly noisier** until RFC-0048's `??` lands;
  sequencing them together is a hard requirement for the ergonomics story.
- **Abort rows get *worse* before RFC-0045**: `days_in_month(y, 13)` goes
  from a wrong-but-running 31 to a bare `wasm unreachable` on the compiled
  backend. Acceptable only because 0045 is in the same tranche; if 0045
  slips, the rule-3 rows should slip with it.
- The `crypto.*_verify` shape change touches security-critical call sites;
  each caller update needs review, not mechanical sed (an `Err` treated as
  `Ok(false)` is safe; the reverse is not — the direction of the `?` matters).

## Prior art

- Rust std: `find`/`position` return `Option`, parsing returns `Result`,
  indexing panics — the same three-way split, written into API guidelines
  (C-QUESTION-MARK et al.). This RFC is that discipline for witchy's ladder.
- Go: one error idiom held by convention and review; witchy's newer stratum
  already beats comma-ok for composition (`?`), which is why finishing the
  migration beats freezing it.
- `book/src/tour-errors.md` — the policy's rules 1–2 were already user-facing
  promises; this RFC makes std keep them.
