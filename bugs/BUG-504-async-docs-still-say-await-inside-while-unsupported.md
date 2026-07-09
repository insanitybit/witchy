# BUG-504: Async docs still say `await` inside `while` is unsupported

Status: FIXED
Verified: 2026-07-09 fixed on master 040ce13b
Severity: LOW
Component: `spec/architecture.md`, `rfcs/concurrency-design.md`, async/await language surface

## Resolution

Current release-facing architecture docs now describe the narrower async gap:
`await` is supported inside loop bodies, including `while` bodies, while
conditions and match scrutinees remain unsupported.

The fix is documentation-only. The implementation already had the supported
state-machine lowering; this row tracked stale prose that made a shipped feature
look unavailable.

## What is wrong

Historical problem: release-facing architecture docs described the async gap as:

> `await` is not yet supported inside a `while` loop or a condition/scrutinee.

That was no longer the current language contract. The shipped lowering supports
`await` in a `while` body, while still rejecting `await` in a `while`
condition, `if` condition, or `match` scrutinee.

This made the language look less powerful and less coherent than it is, and it
blurs a precise remaining limitation into an overbroad "while loops do not work"
warning.

## Evidence

Source:

- `crates/witchy-syntax/src/async_lower.rs` routes a `while` whose condition is
  await-free but body contains `await` through `lower_while(...)`.
- `lower_while(...)` is documented as handling "`while cond:` whose body
  awaits" via a recursive segment loop.
- The same pass still rejects `contains_await(cond)` for `while`, `if`
  conditions, and `match` scrutinees with explicit "not yet supported" errors.

Repro files, kept under ignored `scratch/`:

- `scratch/repro-async-await-while-body.witchy`
- `scratch/repro-async-await-while-condition.witchy`

Commands:

```sh
cargo run --quiet -- check scratch/repro-async-await-while-body.witchy
cargo run --quiet -- scratch/repro-async-await-while-body.witchy
cargo run --quiet -- check scratch/repro-async-await-while-condition.witchy
```

Observed:

```text
scratch/repro-async-await-while-body.witchy: ok
3
link error: async fn `main`: `await` in a `while` condition is not yet supported
```

## Expected fix

Update the async known-gap wording to say that:

- `await` in `while` loop bodies is supported.
- `await` in loop/branch conditions and match scrutinees remains unsupported.
- Any concurrency RFC text retained as current-state documentation should use
  the same narrower wording.

If the RFC text is intentionally historical, mark it as such and keep the
release-facing current-state docs (`spec/architecture.md`, generated book/docs)
precise.
