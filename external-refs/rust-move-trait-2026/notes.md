# Rust 2026 project goal — "Immobile types and guaranteed destructors"

- Source: https://github.com/rust-lang/rust-project-goals/blob/main/src/2026/move-trait.md
- Point of contact: @lcnr; lang champion @jackh726. Status: Accepted (2026–2027).
- Tracking: rust-lang/rust-project-goals#635, rust-lang/rust#149607.

## What it proposes

New auto-traits describing what operations a type permits — the positive framing
"traits are capabilities layered on a base type that has none," following the
`Sized`-hierarchy precedent.

- **`Move`** — opt out of being *relocated in memory*. Encodes immovability as a
  property of the *type*, replacing `Pin` (which makes it a property of the
  *place*). Motivated by self-referential async futures and Linux-kernel pinning.
- **`Forget`** — opt out of `mem::forget`, i.e. **guaranteed destructors**: a
  `!Forget` type's cleanup *must* run. Enables safe scoped-spawn (spawned task
  borrows parent scope and must be joined), transaction commit/rollback, RAII
  guarantees. `unsafe impl !Forget for ScopedTaskHandle {}`.

Notes from the doc: `Move` is deliberately **not** a supertrait of `Destruct`
(want types that can be dropped but not moved). Changing the `Future` trait is
explicitly out of scope. The `Drop` trait gets a one-off `fn drop(&pin mut self)`
overload; new `&pin` lvalue/pattern forms are introduced. The "duplicate
definition problem" (`Trait` vs `PinnedTrait`) is *not* solved.

## Why it matters to witchy

The `Move`/immobile half does **not** transplant: witchy is managed and
address-free (no `Pin`, no `mem::forget`, no observable relocation), so the
motivating problem is absent. The `Forget`/guaranteed-cleanup half **does**: it
is the design witchy lacks for capability handles, scoped task handles, secrets,
and transactions. → informed `rfcs/0114-must-consume-obligations.md`.

## Related prior art cited by the goal

- Baker, "Move, Destruct, Leak" (babysteps, 2025-10-21) — the trait hierarchy.
- Baker, "Must move types" (2023-03-16) — must-consume concept.
- Wuyts, "Ergonomic Self-Referential Types", "Why Pin", "Placing functions".
- rust-for-linux.com, "The Safe Pinned Initialization Problem".
