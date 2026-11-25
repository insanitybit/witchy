# A deque in a ring buffer, and pointwise clauses

`library/wasm-deque.wy` is the second unsafe container written in library code,
after `Vec`. It was written to find out whether the proof foundation is general
or shaped around Vec's filled prefix. It was shaped around the prefix; two
general additions make the ring provable, and nothing in the compiler names
`Deque`.

## The type

```
sealed type Deque<T>:
    data: Buffer<T>
    head: U64
    length: U64
    capacity: U64
    invariant capacity == memory.capacity(data)
    invariant length <= capacity
    invariant head < capacity || head == 0
    invariant forall j in (0, capacity): memory.initialized(data, j) == (head <= j && j < head + length || head <= j + capacity && j + capacity < head + length)
```

The last clause says exactly which cells hold a value: those from `head` for
`length` cells, running past the end of the allocation and on from cell 0.
Arithmetic in clauses is on unbounded numbers, so `head + length` there cannot
overflow, though the same sum in code can.

Operations, each proved against the invariant:

- `push_back_mut`, `push_front_mut`: write one empty cell, unchecked. Err gives
  the item back when the deque is full.
- `pop_front_mut`, `pop_back_mut`: take one full cell, unchecked. Err(2) when
  empty.
- `get(index)`: read the value `index` places from the front, unchecked.
- `at(index)`: a reference to that value, unchecked; `for item in &deque:`
  walks the deque front to back through it (docs/wasm-iteration.md).
- `new`, `try_release`, `close`: allocate, release when empty, take every value
  out and release. `close` uses `memory_take_wrapped`, a loop whose invariant is
  the same pointwise clause over the cells still to be taken.
- `Discard`, for a deque of owners: each value is popped and discarded, then
  the empty deque is released (`tests/fixtures/deque/owners.wy`, Strings).
- `push_back`, `push_front`, `reserve_mut`: the growing forms behind `&mut`,
  built like Vec's from `reserve` and `memory.exchange`; no proof needed.
- `reserve`: not proved, and needs no proof. It builds a new deque and moves the
  values across with `pop_front_mut` and `push_back_mut`, so it never touches
  the fields. Its two `unreachable` arms (a push into a deque with room, a
  release of an empty deque) would trap if the proved contracts were wrong.

A first draft of `push_back_mut` computed `head + length` and wrapped it; Lean
refused it, because that sum can exceed `U64` when the allocation is large. The
written version compares `length` with `capacity - head` instead.

## The two general additions

**`forall j in (lo, hi): body` in contracts.** The parser already read it; no
proof model gave it a meaning. Both memory models (borrowed and owned records)
now accept it in `requires`, `ensures`, loop invariants and type invariants.
The body speaks of single cells (`memory.initialized`, `memory.empty`,
`memory.cell`) and numbers. A quantified clause such as `memory.initialized_range`
is refused there: its own variable would capture the outer one in the statement
(found by an automated review of the first version, which accepted it). One level
only, and the variable cannot hide another name.

**`==` between two truths means "exactly when".** `memory.initialized(data, j)
== (...)` is an equivalence; `!=` is its negation. `memory.initialized` is also
accepted by the owned-record model now, as the negation of `memory.empty`.

## How the proofs go

A pointwise clause is put to use one cell at a time. For a goal about cell `j`
(or about the one cell a step reads or writes), the generated script takes the
clause's instance at that cell, splits on whether the cell held a value before
the step and on whether `j` is the written index, and the rest is arithmetic for
`omega`. Three details were needed for that:

- shared terms are read through again after the instances are taken, so the
  instance and the goal name the same cells;
- an instance whose bound is not known is turned into a disjunction
  (`Decidable.not_or_of_imp`), which `omega` reads and an implication it does
  not;
- the simplifier is given equations only. A lemma stated with `↔` makes `simp`
  add an auxiliary theorem, which the proof audit refuses as uninventoried.

A loop in a borrowed-memory body now knows, at its head, that cells at or past
the capacity are still empty. That holds because every write in the body is
shown to be below the capacity. Without it a loop could not end with
`memory.empty_all`.

## Measured

Fresh Lean time for the deque's 9 proved functions is about 4 s of CPU (the
library and console add 3.8 s for any program). Proofs are cached by
statement, so this is paid once per change. The slowest single goal is
`pop_back_mut` at about 1.6 s. A two-buffer copy loop for `reserve` (take from
the ring, write into a new prefix) was tried and abandoned: its goals ran past
Lean's heartbeat limit. The composition above replaced it.

## Controls

`tests/fixtures/deque/*.refused.wy`, each a copy with one mistake, refused by
Lean: pushing at the back without wrapping, popping the front without moving the
head, reading one past the end, pushing at the front into cell 0 instead of the
last cell, and a new deque that claims one value.

## Still trusted

The same as for Vec: the checker's enforcement of sealing, the translation of
clauses into both models' statements, the borrow checker, and the runtime's
cell layout. The new "empty past the capacity at a loop head" fact is part of the
memory model's translation.
