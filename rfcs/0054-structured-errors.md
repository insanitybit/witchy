---
rfc: 0054
title: Structured errors (design-first)
status: in-progress
created: 2026-07-03
tracking: >
  Core language slice shipped: std/error defines Error: Show, String implements
  Error, and ? accepts/lowers Result(_, Ein) -> Result(_, Eout) through an
  ordinary From(Ein) for Eout impl. std/json and std/toml now expose typed
  decode-error values; std/crypto, std/webauthn, and std/jwt expose typed
  trust-boundary verification errors; the package manager's TUF pinning,
  verified registry-record footprint, and lock snapshot-pin gates use typed
  errors internally, and Coven maintainer-policy state, stored-record parsing,
  source-footprint recomputation, and trusted-publishing token verification have
  typed corruption/authentication errors. All convert to String through From at
  existing application boundaries. Remaining 0.1 work: finish or explicitly
  defer typed errors for other package-manager trust boundaries.
---

# RFC-0054: Structured errors (design-first)

> Design-first, in the manner of `rfcs/externref-implementation-plan.md`:
> this RFC decides the *direction* for error types; implementation is a
> separate, later decision with its own sequencing. No code ships from it.

## Summary

Every error in std is a bare `String`: `json.decode`, `time.parse_iso8601`,
`semver.parse`, `url`, `http.try_get` — all `Result(T, String)`. The one
composition mechanism, `e? "msg"` (desugared to `__try_ctx`,
`crates/witchy-syntax/src/parser.rs:1069-1090`, which prepends context to a
Result's error and bridges `Option` → `Result(T, String)`), is anyhow-shaped:
excellent for applications, a ceiling for libraries. This RFC works through the
design space and recommends **(a) errors as ordinary enums + a std `Error`
trait + `From`-based `?` conversion** — adopted as direction now, implemented
only after RFC-0046 lands typed trait dispatch, with std migration sequenced
behind demonstrated demand rather than a schedule.

## Motivation

A string error can be *displayed* but not *matched*. A caller of a hypothetical
`fetch_config` rune that wants "retry on timeout, fail on parse error" must
today match on substrings of a message the library never promised as API. Rust
walked exactly this road: `String`/`Box<dyn Error>` errors → typed error enums →
the anyhow/thiserror split, which is really one insight — **applications want
one opaque, contextful error; libraries want a matchable type** — and witchy
already has the application half built in (`?` + `? "msg"` + didactic
main-returns-Result rejection, `crates/witchy-types/src/typeck.rs:650`).

What forces the question now is **coven**: a package ecosystem is a library
ecosystem. The consistency analysis (2026-07-03) found the error surface is
"an unfinished migration wearing a Rust costume" — and its written-policy tier
(RFC-0044) fixes the *shape* violations (sentinels → Option, silent defaults →
aborts) but deliberately keeps `Result(T, String)`. This RFC is the next layer:
what replaces `String` when a library needs more.

Constraints, all non-negotiable:

- **Parity** — errors are ordinary values; both backends already agree on them.
  Nothing here may introduce a backend-divergent error representation.
- **No exceptions, ever** — already doctrine; errors stay values in `Result`.
- **`? "msg"` keeps working** — it becomes sugar *within* the new scheme, not a
  casualty of it.
- **`main` never returns `Result`** — the exit-code idiom stands.

## Design (the space, then the recommendation)

### (a) Errors as ordinary enums + convention + conversion — recommended

Everything needed to *define* a typed error already exists:

```
type ConfigError:
    NotFound(String)
    ParseFailed(String)
    Timeout(Int)
```

`Result(Config, ConfigError)`, matched with `match` — expressible today. What
is missing is not a type feature; it is **convention plus conversion**:

1. **A std `Error` trait.** Minimal: `Show` as a supertrait (every error can
   render), plus an optional `fn source(self) -> Option(...)` accessor for a
   cause chain, added only if chains prove wanted. The trait's real job is to
   be the *bound* — `Result(a, e) where e: Error` — that lets combinators and
   the CLI print any error without knowing its type. Supertraits already exist
   (`std/cmp.witchy:33`), so this is one small std module.
2. **`?` performs `From`-based conversion.** The Rust model: when the
   surrounding function returns `Result(T, E)` and the operand is
   `Result(T2, E2)`, `?` inserts `E.from(err)` if `E2 != E` and an
   `impl From(E2) for E` exists. witchy already ships `From`/`Into` with the
   blanket impl (`std/convert.witchy`) — the mechanics exist; what's missing
   is the type-directed insertion in the `?` desugar. **This is the piece
   gated on RFC-0046**: today's string-shaped dispatch
   (`recover_generic_call`) cannot reliably resolve "which `From` impl" at the
   `?` site; threading typeck's real TypeTable through dispatch (0046) is the
   prerequisite. Building it on the shadow type system would add exactly the
   kind of per-shape special case 0046 exists to delete.
3. **`? "msg"` becomes context-wrapping in the scheme.** For
   `Result(T, String)` it keeps today's prepend semantics verbatim (no
   migration). For a typed `E: Error`, it wraps: the error converts to the
   application-level string form via `show`, with the message prepended —
   i.e. `? "msg"` *is* the typed→anyhow boundary, exactly where Rust puts
   `.context(...)`. One operator, both worlds.
4. **Std grows typed errors module-by-module, behind demand.**
   `json.DecodeError` first — highest value (position + expected/found are
   begging to be fields, and json feeds the crypto/jwt stack), clearest
   litmus test for the ergonomics. Each migration is a breaking change per
   the break-don't-deprecate rule; nothing migrates on a schedule.

### (b) A universal std `ErrorValue` record

One std record — `message: String, code: String, source: Option(ErrorValue)` —
everywhere. Weighed: it gives uniform rendering and a cause chain *now*, with
no trait machinery and no 0046 dependency. But matching on a stringly `code`
field is string-matching with extra steps — the central defect survives; it
forecloses payload-carrying variants (`Timeout(Int)`); and it would ossify into
exactly the compat layer break-don't-deprecate forbids once (a) becomes
possible. Rejected as the destination; nothing stops a rune from using this
shape privately in the meantime.

### (c) Keep `String`, add nothing — genuinely weighed

The honest case: witchy's posture today is application-first; `? "msg"` chains
compose well and read well; every probe of real witchy code (pm, coven,
glamour) shows the string form serving fine; and Rust survived *years* on
strings before typed errors mattered. The moment (a) becomes right is when
independent runes need to react to each other's failure *modes* — which is a
coven-maturity milestone, not a language-schedule one. This alternative loses
only as a *permanent* answer; as a *timeline* it is largely correct, and the
recommendation below adopts its patience.

### Recommendation

**Adopt (a) as direction; implement nothing until RFC-0046 lands; then ship
the `Error` trait + `From`-inserting `?` as one increment; migrate std
module-by-module behind demand, starting with `json.DecodeError`.** (c)'s
timeline discipline, (a)'s destination. RFC-0044's policy rules stay the
governing document for error *shape* (Option vs Result vs abort); this RFC
governs what the `e` in `Result(T, e)` may become. Structured errors also
compose with RFC-0045: a typed error that reaches an abort should render
through the same message channel compiled traps gain there.

## Alternatives

Covered as (b) and (c) above — (b) rejected, (c) absorbed into sequencing. A
fourth option, checked exceptions / effect-typed errors, is rejected without a
section: it is hidden control flow, which the error doctrine (`book/src/
tour-errors.md`) explicitly forbids.

## Drawbacks

- **Two error idioms during the (long) transition** — `Result(T, String)` and
  typed errors will coexist for years. Mitigated by `? "msg"` being the
  designed bridge, and by RFC-0044's written policy naming which stratum a
  function belongs to. The consistency analysis showed unlabeled strata are
  the real cost; this RFC at least labels them.
- **Dependency risk:** the recommendation is gated on RFC-0046, which is the
  largest open compiler item. If 0046 slips, this RFC delivers nothing — an
  accepted consequence of refusing to build on the shadow type system.
- **Ecosystem fragmentation risk:** two runes defining incompatible
  `TimeoutError`s. The flat type namespace (RFC-0042's subject) makes this
  worse before it makes it better; module-qualified error types should be the
  convention from the first std migration.
- **Every std migration is a breaking change.** Deliberate (break-don't-
  deprecate), but each one costs downstream churn — hence "behind demand."

## Prior art

- **Rust**: `std::error::Error`, `?`'s `From` insertion, and the
  anyhow/thiserror split — the direct model for (a) and for `? "msg"` as the
  context boundary. Rust's history (strings → typed → the split) is the
  argument for (c)'s patience.
- **Go**: `errors.Is`/`As` + `%w` wrapping — the same convergence (opaque
  chains for apps, matchable types for libraries) reached from the opposite
  starting point.
- Internal: RFC-0044 (error/return policy — shape), RFC-0046 (typed trait
  dispatch — the gate), RFC-0045 (compiled trap diagnostics — the render
  channel), `std/convert.witchy` (From/Into), consistency-analysis §5 (the
  evidence base).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below (e.g. "> 2026-07-01: clarified X").
  - The current behavior lives in spec/ and the code — NOT here.
-->

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** The direction is right — errors as enums + an
Error trait + From in `?`, with `? "msg"` as the typed→string boundary — and the
rejection of a universal ErrorValue is well-argued. Status quo verified: 33
`Result(T, String)` signatures in std, zero typed errors. The gate language is
stale: the named prerequisite (RFC-0046 TypeTable dispatch) has merged; the real
gate is now DEMAND — a coven ecosystem reacting to failure modes — which hasn't
materialized.

**Required revisions.** Update the gate language (demand-gated, no longer
capability-gated). Answer before any implementation: `? "msg"` semantics inside
typed-E functions; the Option operand under typed E; `source()` stability. Do
not start `json.DecodeError` yet.

**Verdict.** Accept as direction; defer implementation. Priority: medium
(acceptance) / low (implementation).

## Implementation note (2026-07-07)

The core language mechanism is implemented. `std/error.witchy` provides the
conventional `Error: Show` bound and keeps `String` in the error family for the
existing application-style surface. The type checker now accepts `expr?` in a
`Result(_, Eout)` function when `expr` is `Result(_, Ein)` and the lowered module
contains an ordinary `impl From(Ein) for Eout`. Trait lowering rewrites that
operand to `result.map_err(expr, fn(e: Ein): Eout.from(e))` before either backend
sees it, so interpreter and compiled WASM share one AST and the old `?` lowering
still only propagates same-error `Result`s.

The Option case is deliberately narrower: plain `Option(T)?` still only
propagates through an `Option`-returning function, because a bare `None` carries
no error value to convert. The contextual form, `opt? "message"`, remains the
typed-error bridge: it turns `None` into `Err(String)`, and the same
`From(String) for E` rule lets a typed `Result(_, E)` function accept it when the
library author explicitly provides that conversion.

`std/json.decode` now returns `Result(Json, json.DecodeError)`, and
`std/toml.decode` now returns `Result(Toml, toml.TomlDecodeError)`. The
authentication/verification trust boundary has also moved: `std/crypto`
verifiers return `crypto.VerifyError`, `std/webauthn.verify_assertion` returns
`webauthn.AssertionError`, and `std/jwt` verification/JWKS helpers return
`jwt.JwtError`. These typed errors implement `Show`, `Error`, and conversion to
`String`, so libraries can match typed errors while existing stringly
application boundaries can keep using `?` or render the message explicitly with
the module helper (`json.decode_error_message`, `toml.decode_error_message`,
`crypto.verify_error_message`, `webauthn.assertion_error_message`, or
`jwt.jwt_error_message`).

The package manager's TUF pinning boundary is also typed internally:
`snapshot_pin` / `verified_snapshot_line` return a local `TufError` after
checking the registry root key, timestamp/snapshot signatures, role schemas,
snapshot hash binding, and version agreement. CLI-facing code still renders
the existing messages before refusing to write unverified trust metadata.
The PM widening gate's verified registry-record footprint boundary now also
uses a local `RecordFootprintError` covering root-key mismatch, missing record,
bad signature, coordinate mismatch, malformed JSON, missing/non-array
`runtime_footprint`, and non-string footprint elements. Coven maintainer-policy
state uses `coven_trust.TrustPolicyError` for invalid JSON, non-array policy
state, and non-string maintainer entries before the server maps corruption to a
500 `CovenError`. The PM lockfile's `registry_snapshot_version` parser now
returns a local `LockPinError` for non-integer or negative pins before `pm verify`
renders the existing `BLOCK:` diagnostic. Stored Coven records parse through
`coven_record.RecordParseError`, keeping malformed JSON distinct from a
well-formed JSON value with the wrong record shape before the registry maps it
to a corrupt-record response. Coven source-footprint recomputation returns
`coven_footprint.FootprintError`, so malformed compiler footprint reports and
uncompilable source are matchable before publish maps them to the existing 400
response. Coven trusted-publishing token verification returns
`coven_trust.TokenError`, preserving the distinction between malformed identity
tokens, untrusted issuers, missing JWKS `kid`, JWKS key-selection failures, and
OIDC claim/signature rejection before the server maps them to 401 responses.

This does not complete the RFC. The remaining release-blocking work is the std
and core-library migration: other package-manager trust boundaries still need
typed decoder errors or an explicit 0.1 deferral, and broader convenience
parsers/codecs (`encoding`, `url`, `semver`, `time`, `http`) remain string-error
APIs until demand justifies their own typed cuts.
