---
rfc: 0114
title: Must-consume obligations (linear resource handles)
status: implemented
created: 2026-08-03
superseded-by:
tracking: >
  Implemented 2026-08-20 across syntax, the CFG-aware checker, aggregates,
  transactions, and structured tasks. `must type`,
  `must sealed type`, and `must capability` preserve declaration metadata;
  all-path disposition, move/copy, overwrite, borrow, aggregate propagation,
  own-call attempt semantics, suspension-frame transfer, and must-consume task
  handles have executable checker and compiled-Wasm coverage. The standard
  `transaction` resource now proves success, conflict, rollback, branch, move,
  and aggregate behavior through the checker and compiled Wasm, plus rejection
  when an early return would abandon the resource. The serialized workspace
  matrix passed after both standard-library integrations merged.
---

# RFC-0114: Must-consume obligations (linear resource handles)

## Summary

This RFC adapts one half of Rust's 2026 ["Immobile types and guaranteed
destructors" project goal](../external-refs/rust-move-trait-2026/notes.md) to witchy. Rust
proposes two new auto-traits: `Move` (types that opt out of being relocated in
memory) and `Forget` (types that opt out of `mem::forget`, guaranteeing their
destructors run). We adopt **only the `Forget` half**, reframed as a witchy
ownership convention: a value may carry a `must` obligation — the compiler
refuses to let it be implicitly dropped, forcing it to flow into a consuming
operation (`close`, `commit`, `join`, `zeroize`, …) before it leaves scope. We
explicitly **reject** the `Move`/immobile-types half: witchy has no `Pin`, no
exposed addresses, and a managed representation, so the problem it solves does
not exist here.

## Motivation

### The half that does not apply: `Move`

Rust's `Move` trait exists because Rust values are relocated by raw
byte-copy, which breaks self-referential types; `Pin` patches this by making
immovability a property of *places*, at great complexity cost, and the async
`Future` state machine is the headline victim.

None of that structure exists in witchy:

- Values live in WASM linear memory / externref behind a managed representation
  (`spec/architecture.md`). Programs never observe an address, so "this value
  moved in memory" is unobservable and cannot break a self-reference.
- witchy's async is a compiler-generated state machine (RFC-0059); the compiler
  owns its layout and never needs a user-facing pin.
- There is no `mem::forget`, no `unsafe` self-referential construction.

So we do **not** introduce a `Move` trait or immobile types. The motivation is
absent, and adding the machinery would be pure cost. (Recorded here so a future
reader does not re-derive the question.)

### The half that does apply: guaranteed cleanup

witchy today has ownership annotations (`let`/`var`/`own`), the `unique`/`frozen`
contract qualifiers, structured concurrency with scoped spawn, and capability
handles (File, Net, secrets, coven/pm transactions). What it lacks is any way to
*guarantee* such a handle is finalized. There is no `defer`, no destructor, no
`!Forget`. A resource is released implicitly whenever the reclamation floor
(RFC-0016/0035) decides its last use passed. That is fine for pure data and
wrong for obligations:

- A **capability handle** should be explicitly `close`d (or a secret
  `zeroize`d), not silently reclaimed in nondeterministic order.
- A **scoped task handle** must be `join`ed before its scope exits — the exact
  precondition that makes safe "spawn a task that borrows the parent scope"
  sound. This is the same win Rust cites for `!Forget`.
- A **transaction** (coven publish, pm lockfile write) must reach `commit` or
  `rollback`; dropping it half-done is a latent bug.

Without a compile-time obligation, every one of these is a runtime convention
enforced by code review.

## Design

### The `must` qualifier

Introduce one new value obligation, spelled `must`, orthogonal to `let`/`var`/`own`
and expressed the same way the existing conventions are — as a fact the
escape/uniqueness analysis (`crates/witchy-lower/src/analysis.rs`, and the loan
checker in `witchy-types/src/loans.rs`) consumes uniformly. **No per-type special
casing** (per the standing "generalize, never special-case" rule): `must` is a
qualifier on a binding/type, not a registry of blessed handle types.

A `must` value obeys one rule:

> A `must` value may not reach the end of its scope, be overwritten, or be
> discarded (`_ = expr`) while still live. It must be *consumed* — moved
> (`own`-passed) into a function that takes it by `own`, or returned. Consuming
> functions are ordinary; there is no destructor trait.

```
# a handle whose type is declared must-consume
must sealed type Txn:
    Txn(Int)

fn open_txn(own store: Store) -> Txn
fn commit(own txn: Txn) -> Result(Nil, Error)   # consumes the obligation
fn rollback(own txn: Txn)                        # also consumes it

fn publish(own store: Store) -> Result(Nil, Error):
    var txn = open_txn(store)
    # ... do work ...
    commit(txn)        # OK: obligation discharged
    # if we forgot this line, compile error:
    #   `txn` still holds a must-consume obligation at end of scope
```

Branches must discharge on **every** path (mirrors witchy's existing
definite-assignment analysis): if one arm `commit`s and the other falls through,
that is the same error. A `must` value moved into a `?`-propagated call is
consumed on both the ok and error edges (RFC-0087 already commits callee-`?`
effects, so the obligation is genuinely gone).

### Declaring an obligation

A type carries the obligation by annotating its declaration, not by implementing
a trait:

```
must type Txn:
    Txn(Int)                 # every Txn value is must-consume
```

This keeps the base type "capability-free" and layers the obligation on top —
the positive framing Rust uses ("traits are capabilities on a base with none"),
minimized to the single axis witchy actually needs: *droppable* vs
*must-consume*. We do not port Rust's full `Copy`/`Move`/`Forget`/`Destruct`
lattice; witchy needs exactly this one bit.

### Parity story

This is a **static, pre-backend** check, exactly like RFC-0083 borrowed views
and RFC-0089 FIP: the `must` qualifier is erased after checking and both backends
receive identical lowered WIR. There is **no runtime finalizer, no drop glue, no
GC callback** — those would be a nondeterministic parity hazard and are rejected.
The obligation is discharged by an ordinary consuming call that already runs
identically on both backends. Parity impact: a program either compiles (and both
backends behave identically) or is rejected on both with the same diagnostic.

### Interaction with structured concurrency

The scope handle returned by a structured `spawn` becomes a `must` value whose
only consumer is the scope's `join`/close. This lets the checker prove every
spawned task is awaited before its borrowed scope dies — enabling the
borrow-from-parent scoped-spawn pattern safely, without a lifetime system.

## Alternatives

- **Do nothing.** Keep runtime conventions + review. Costs: the transaction /
  scoped-task / secret-zeroize bugs stay latent and un-catchable. Rejected.
- **Runtime destructors (`defer` / drop glue that auto-runs on scope exit).**
  Ergonomic (Rust's actual `Drop`) but introduces nondeterministic ordering and
  a genuine twin-backend divergence surface (drop order, drop-during-panic). A
  possible *phase 2* behind a separate RFC, but the static must-consume form
  delivers the guarantee with zero runtime and zero parity risk, so it is
  phase 1 alone.
- **Adopt `Move`/immobile types too.** No motivating problem in a managed,
  address-free runtime (see Motivation). Rejected.
- **A blessed registry of handle types with hard-coded cleanup.** Violates
  "generalize, never special-case"; would silently miss every new handle (the
  same failure mode as the `dict_remove_cap` leak). Rejected in favor of the
  uniform `must` qualifier.

## Drawbacks

- A new obligation axis in the checker and the uniqueness/escape analysis, plus
  new diagnostics ("must-consume value not discharged on all paths").
- `must` values cannot be stored in ordinary collections without the collection
  itself propagating the obligation. Nominal aggregates, tuples, and lists now
  propagate it transitively; moving individual values back out of containers
  remains deliberately restricted until their extraction APIs preserve affine
  identity.
- Ergonomic gap vs. auto-drop languages: the programmer writes the consuming call
  explicitly. Accepted as the price of parity safety.

## Prior art

- Rust 2026 project goal, *Immobile types and guaranteed destructors* (@lcnr) —
  the `Move`/`Forget` split. We take `Forget`, drop `Move`. See
  `external-refs/rust-move-trait-2026/notes.md`.
- Baker, *Move, Destruct, Leak* (babysteps, 2025-10-21) — the trait hierarchy;
  note their observation that `Move` need not be a supertrait of `Destruct`.
- Baker, *Must move types* (2023) — the must-consume concept this mirrors.
- witchy RFC-0083 (borrowed views), RFC-0087 (fused mutators / callee-`?`
  commit), RFC-0089 (FIP contract) — the static, erased-before-backend pattern
this follows.

## Acceptance ledger

- PROVEN: declaration syntax and formatting preserve `must` metadata.
  Evidence: `must_consume_marker_is_nominal_declaration_metadata` and
  `must_consume_declarations_survive_formatting`.
- PROVEN: scope exit, overwrite, implicit copy, explicit transfer, return, and
  all-path branch joins enforce one live obligation per binding identity.
  Evidence: `must_consume_requires_disposition_on_every_path`,
  `must_consume_transfers_without_copying_and_propagates_through_aggregates`,
  `must_consume_own_calls_discharge_at_attempt_and_shadowing_keeps_binding_identity`,
  and `owned_function_values_transfer_must_consume_closure_captures`.
- PROVEN: borrowed must-consume values retain the caller's obligation and may
  not be consumed by the callee; unbound borrowed temporaries are rejected.
  Evidence:
  `must_consume_borrows_require_a_live_owner_and_only_own_operations_may_destructure`.
- PROVEN: the standard `transaction.Transaction` resource can only be finished
  by crossing an explicit `own` boundary. Its `commit` success and conflict,
  `rollback`, all-path branch, explicit move, implicit-copy rejection,
  aggregate propagation, and early-return loss execute through the ordinary
  checker and compiled-Wasm paths. Evidence:
  `transaction_resource_consumes_success_conflict_rollback_moves_aggregates_and_cfg_early_return_on_wasm`
  and
  `transaction_resource_rejects_lifecycle_loss_on_scope_branch_move_and_aggregate_paths`.
- PROVEN: CFG joins exclude ownership state from terminating `if` branches and
  `match` arms, so a resource consumed before an early return remains available
  to a later fallthrough consumer. Every terminating path must still discharge
  its live obligations. Evidence:
  `must_consume_cfg_join_excludes_terminating_branches`,
  `transaction_resource_consumes_success_conflict_rollback_moves_aggregates_and_cfg_early_return_on_wasm`,
  and
  `transaction_resource_rejects_lifecycle_loss_on_scope_branch_move_and_aggregate_paths`.
- PROVEN: `task.Handle` and `List(task.Handle)` are must-consume; `join`,
  `cancel`, `join_all`, and `cancel_all` are consuming operations. Plain drop,
  early return, aggregate drop, and one-path disposition are rejected, while
  all-path join/cancel checks and aggregate execution completes on compiled
  Wasm. Evidence:
  `task_handles_must_be_joined_cancelled_or_returned_on_every_path` and
  `structured_task_handle_aggregates_run_on_compiled_wasm`.
- PROVEN: the serialized workspace matrix passed after the transaction and
  structured-task integrations merged. The focused evidence above remains in
  the ordinary gate, and the subsequent typed-carrier landing `8b32e13e`
  passed that complete gate on current master.

---
