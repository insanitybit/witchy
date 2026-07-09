# BUG-536: Contextual `? "msg"` docs overstate typed-`Result` support

Severity: LOW
Status: FIXED
Verified: 2026-07-09 fixed on master 16a1b98b
Component: `?` operator, typed `Result`, structured-error docs, parser comments

## Resolution

Current source now states the actual 0.1 contract instead of the older
"wherever bare `?` does" overclaim:

- `spec/language.md` says bare `e?` propagates `Option(T)` or `Result(T, e)`
  unchanged.
- `spec/language.md` says contextual `e? "msg"` is the string-error convenience:
  it accepts `Option(T)` or `Result(T, String)` and yields `Result(T, String)`.
- `crates/witchy-syntax/src/parser.rs` describes the desugar as `__try_ctx`,
  which turns an `Option(T)` or `Result(T, String)` into `Result(T, String)`.
- `src/example_tests.rs` has RFC-0054 tests covering typed `?` conversion
  through `From`, typed `Option ? "msg"` conversion through `From(String)`, and
  rejection when plain `Option ?` would have to invent a typed error.

Focused verification:

```sh
rg -n 'works wherever bare' spec/language.md crates/witchy-syntax/src/parser.rs
rg -n 'string-error convenience|Option\\(T\\).*Result\\(T, String\\)|__try_ctx' spec/language.md crates/witchy-syntax/src/parser.rs
```

The stale phrase is gone; the current contract is documented in both places.

## Summary

Historical problem: bare `?` works with a non-`String` error type such as `Result(Int, MyErr)`, but
contextual `? "msg"` intentionally supports only `Option(T)` and
`Result(T, String)` because it prepends text and yields `Result(T, String)`.

The implementation was coherent, and RFC-0054 already recorded the typed-error
future. The release-facing gap was that `spec/language.md` and the parser
comment said the message form "works wherever bare `?` does", which was false
for typed `Result` values that bare `?` currently accepts.

## Reproduction

Bare `?` with a typed error checks:

```witchy
type MyErr:
    Bad

fn step(ok: Bool) -> Result(Int, MyErr):
    if ok:
        Ok(7)
    else:
        Err(Bad)

fn pipeline(ok: Bool) -> Result(Int, MyErr):
    let x = step(ok)?
    Ok(x + 1)
```

Verified:

```text
$ cargo run --quiet -- check scratch/repro-typed-result-bare-question.witchy
scratch/repro-typed-result-bare-question.witchy: ok
```

Adding a context message rejects the same typed error:

```witchy
type MyErr:
    Bad

fn step(ok: Bool) -> Result(Int, MyErr):
    if ok:
        Ok(7)
    else:
        Err(Bad)

fn pipeline(ok: Bool) -> Result(Int, MyErr):
    let x = step(ok)? "running step"
    Ok(x + 1)
```

Verified:

```text
$ cargo run --quiet -- check scratch/repro-typed-result-context-question.witchy
type error: `repro-typed-result-context-question.pipeline`, line 11: `? "msg"` prepends to a String error, so the `Result`'s error type must be `String`
```

## Source Evidence

- `spec/language.md:806-810` says contextual `?` "works wherever bare `?` does",
  then describes a propagated `String` error.
- `crates/witchy-syntax/src/parser.rs:1305-1314` repeats the same overclaim in
  a parser comment: "works wherever bare `?` does".
- `crates/witchy-types/src/typeck.rs:2052-2092` documents and enforces the
  actual rule: `__try_ctx` accepts `Option(T)` or `Result(T, String)` and returns
  `Result(T, String)`.
- `rfcs/0054-structured-errors.md:17-24` describes current `? "msg"` as
  application/anyhow-shaped and `Result(T, String)`-oriented.
- `rfcs/0054-structured-errors.md:88-93` explicitly makes typed-error context
  wrapping future work, not current behavior.

## Why This Matters

This is small, but it is exactly the kind of public-doc mismatch that makes a
young language feel less precise than it is. The underlying design is reasonable:

- bare `?` is generic over the enclosing `Result`/`Option` shape;
- `? "msg"` is an application-oriented convenience that normalizes to
  `Result(_, String)`;
- richer typed context is deferred to RFC-0054.

The bad part is only the promise boundary. A user experimenting with a typed
error enum can successfully use bare `?`, read that contextual `?` works
wherever bare `?` does, and then hit a type error. That makes typed errors feel
accidental even though the current limitation is intentional.

## Suggested Fix

For the 0.1.0 release, update the spec and parser comment to say something like:

> Bare `e?` propagates `Option(T)` or `Result(T, e)` unchanged. The contextual
> form `e? "msg"` is the String-error convenience: it accepts `Option(T)` or
> `Result(T, String)` and yields/propagates `Result(T, String)`.

Leave RFC-0054 as the direction for typed-error context wrapping.
