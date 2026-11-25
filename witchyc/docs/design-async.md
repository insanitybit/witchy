# Design: suspension without `async`

The reference for what is built is docs/wasm-tasks.md.

Status: decided 2026-11-16 (docs/decisions.md); being built in the order below.

Original Witchy's async (a compiler-owned scheduler, task recipes and replay, quanta,
close passes, generated channel templates) is a record of what not to do. Here the
compiler learns one thing, how a computation that can pause becomes a state-machine
record, and everything else is library code the ordinary checker sees.

## The rules

- **Suspension is an effect, not a colour.** A host operation of an event-loop capability
  may suspend; a function that calls one may suspend. There is no `async` and no `await`:
  calls run now, in order, and a call that pauses pauses its task.
- **Checked once, from the bounds.** In `fn copy<R: Read>(r: &mut R, ...)` whether
  `r.read_some(...)` may pause belongs to `R`; the body is checked once against that, never
  by looking at the types it is later used with. (Deciding per instance was proposed and
  rejected: a generic function holding a lock guard would be refused only at some distant
  use.)
- **`suspends` on public signatures.** A `pub` function that is not generic states that it
  may pause; private functions are inferred, so editing a private body cannot change a
  public promise.
- **`nosuspend`** as a bound (`R: Read nosuspend`) and a block: a value that must not live
  across a pause (a lock guard) is refused across a possible one, cleanup included.
- **Computations not yet started are closures.** `join(fn(): a(), fn(): b())`; the sugar
  `concurrently:` runs each statement of its block as a branch of that join, and `first:` is
  the race. Both are rewrites to library calls.
- **Fixed values.** A started computation is *fixed*: made in its place, never moved,
  returned, stored or given as a type argument; reached through `&mut`, ended by a consuming
  step. It is the existing "no move while lent" made permanent, in place of Rust's `Pin`.
  Users meet it only in library code like `join`.
- **Scopes are `using`.** `using group = tasks.group(&allocator, 64):` drains its children
  on every exit, because a group's `Discard` may pause. A task may borrow what outlives its
  group; a group cannot be forgotten, so this is sound where Rust's scoped spawn is not.
- **Cancellation is a call**, `child.cancel()`: the task's pauses return CANCELLED and it
  winds down through its own code, closing what it holds. Nothing is cancelled by going out
  of scope.
- **I/O that may pause owns its buffer**; synchronous bulk I/O borrows `&mut [U8]`.

## Order

1. `&mut [T]`. **Built** (docs/design-slices.md).
2. Bulk synchronous host I/O with partial progress. **Built**: `read_some`/`write_some`/
   `write_all` on sockets, `read_some`/`write_bytes` on files; a range crosses as
   `address | length << 32` and the result is `count | error << 32`.
3. A synchronous TCP echo against native Go: the baseline. **Done**
   (`scripts/bench-echo.py`; loopback, one connection, the same Python client for each,
   median of three on 2026-11-17):

   | Server | 64 B round trip | 4 KiB round trip | 4 MiB stream |
   |---|---|---|---|
   | Witchy, bulk | 60 us | 57 us | 1,386 MB/s |
   | Witchy, a byte per call | 61 us | 107 us | 76 MB/s |
   | Go | 63 us | 63 us | 1,676 MB/s |

   Small round trips are bound by the Python client. Getting here also fixed two host
   costs: no `TCP_NODELAY`, and a flush (one `write`) before every byte received.
4. Generators holding owners across a `yield`. **In part**: any number of owned parameters,
   handed back by `finish`. Owner *locals* across a `yield` need flow facts the parser does
   not have; they come with the typed lowering of step 7.
5. The `suspend` effect. **Built** (`src/compiler/effects.rs`, `tests/fixtures/effects/`):
   `-> T suspends` on functions and trait methods, `R: Read nosuspend`, `nosuspend:` blocks.
   Each function gets a summary, "pauses" plus `(type parameter, trait method)` pairs it
   pauses through, computed once per generic body and substituted at calls, to a fixed
   point for recursion. Checked: a `pub` function that may pause says so (an implementation
   of a trait method is governed by its trait instead); an implementation pauses only where
   its trait method is `suspends`; a `nosuspend` parameter is given a type whose methods
   never pause (or another `nosuspend` parameter); no call in a `nosuspend:` block may pause.
   Closures' call trait is `suspends`, so a closure's effect follows its body through
   generic code. Nothing pauses at run time yet: the check is what makes the lowering of
   step 7 safe to add.
6. Non-blocking host operations and an event loop; echo again, concurrently. **Done**, in
   plain code over a `Group` (docs/wasm-capabilities.md): `wait` is `poll`, sockets have
   `try_read`/`try_write`, listeners `try_accept`. `examples/echo/server.wy`, one thread,
   against a Go server with a goroutine per connection, the same Go load client checking
   every byte (`scripts/bench-echo-concurrent.py`, median of three, 2026-11-17):

   | Case | Witchy (1 thread) | Go, GOMAXPROCS=1 | Go |
   |---|---|---|---|
   | 1 conn x 4000 x 64 B | 26,344/s, p99 103 us | 27,247/s, 96 us | 26,147/s, 103 us |
   | 32 conns x 1000 x 64 B | 102,912/s, 642 us | 106,130/s, 845 us | 137,270/s, 551 us |
   | 128 conns x 250 x 64 B | 108,692/s, 1,884 us | 110,236/s, 1,832 us | 149,955/s, 1,959 us |
   | 32 conns x 250 x 4 KiB | 86,830/s, 979 us | 87,175/s, 1,122 us | 107,081/s, 606 us |

   One thread of Witchy is within 3% of Go on one core; Go on every core is 25-40% ahead,
   which more stores (threads) would address, not a faster loop. Step 7 makes such a server
   ordinary sequential code per connection; this one is written as an event loop.
7. Tasks and the lowering that pauses them. **Built** (below, "How a pause works";
   `tests/fixtures/tasks/`). Sockets have `read` and `write` that pause the task, with a
   wait entry for the socket as the token, and `listener.poll` waits on many entries.
   `examples/echo/tasks.wy` is the echo server as one sequential function per connection;
   the same benchmark (median of three, 2026-11-17):

   | Case | Event loop | Tasks | Go, GOMAXPROCS=1 | Go |
   |---|---|---|---|---|
   | 1 conn x 4000 x 64 B | 26,414/s | 25,186/s | 27,114/s | 26,657/s |
   | 32 conns x 1000 x 64 B | 103,410/s | 92,524/s | 107,666/s | 132,754/s |
   | 128 conns x 250 x 64 B | 108,241/s | 93,185/s | 110,242/s | 150,860/s |
   | 32 conns x 250 x 4 KiB | 91,112/s | 79,907/s | 91,860/s | 109,397/s |

   `main` does not write the stepping itself: `Waiting` (library/wasm-net.wy) keeps the
   waiting tasks and their entries, `start` steps a new one, `step_ready` polls once and
   steps every task that is ready, and a task hands results back with `report(value)`.

   Tasks cost 5-14% against the hand-written loop in that run. Not in the pause itself: a
   pause saves only the locals live after its site (and the conditions going back in reads),
   and a million pauses and resumptions through three frames take 30 ms (`spin` in the commit
   that made saves live-only; 40 ms saving every local). It was a second `recv` per message,
   always empty, and a buffer allocated and copied per call. The host now answers the `recv`
   after a short read "nothing yet" without the system call (once; a poll that finds the
   socket readable clears it) and reads and writes the program's memory directly; in a run on
   a busy machine the task server was then even with the event loop (92,246/s against
   88,864/s at 32 connections, 91,943/s against 92,008/s at 128; 93% with 4 KiB messages).

   A task waits on several things by pausing with `ALSO + entry` for all but the last (the
   stepping code records each and steps it again at once). `join(&mut [Task])` (library/
   wasm-tasks.wy) runs up to 32 tasks inside the running one this way, and `Waiting` steps a
   task when any of its entries is ready (`tests/fixtures/tasks/join.wy`, `net/join.wy`).
   A pause returns the word the step that continues it hands in (`pause_for`): `Waiting`
   hands in the entry found ready, so `join` steps only the tasks waiting on it, and
   `task.cancel()` hands in CANCELLED, which the library's pausing operations turn into
   `Err(Cancelled)` so a task winds down through its own code (`tests/fixtures/tasks/
   cancel.wy`, `join-wake.wy`, `net/cancel.wy`). `first(&mut [Task])` races tasks and
   cancels the losers. Timers are wait entries too: `clock.sleep(ms)` pauses on one, and a
   timeout is `first` over the work and a sleeper (`net/sleep.wy`). A server shutting down
   calls `waiting.cancel_all()`; a client steps its tasks through its `Net`
   (`Waiting.empty`, `step_ready_net`; `net/client-tasks.wy`). The `first:`/`concurrently:` blocks wait for closures that
   can carry capabilities into tasks.

## How a pause works

A task is a function and one owned input, `Task.spawn(&allocator, bytes, serve, move
socket)`, run on its own stack in one heap block: a header, the input, a save area and the
stack. Nothing in the block refers outside it, so the `Task` handle can move anywhere, a
`Vec` included: there are no fixed values and no `Pin`. `task.step()` runs it until it
finishes (`Ok`) or pauses (`Err(token)`); `pause(token)` ends a step, and the token means
what the stepping code makes it mean. `task.close()` gives a finished task's memory back;
a task must be closed, and closing one that is paused traps.

A pause unwinds and a step rewinds, in the emitted code only (src/compiler/pause.rs, emit.rs),
in the functions that may pause and nowhere else:

- Each call that may pause (a *site*) is its own statement, its arguments in locals, and
  what was evaluated before it evaluated first, so the source's order holds.
- After a site, if the call paused, the function saves every local and the site's number
  to the save area and returns. A step that finds the task paused calls its function again,
  which restores its locals and skips every statement but the one holding that site, down to
  the pause, which returns. Conditions and scrutinees are locals, so going back in takes the
  branches that were taken.
- The stack frames of a running task are bounded by its own stack (globals a step moves), so
  a reference into a frame stays valid while the task is paused.

Only what can pause pays: a function that cannot is emitted as before, and a program without
tasks is the same module as before. No proof sees any of this: a pause is invisible to the
function that pauses (nothing outside a task refers into it), and a step is a call that
reaches only what the task owns. A function with a contract may not use the task operations
itself (`memory.pause`/`step`/`finish`/`share`), which have no proof model; it may call
library functions that do, like any call without a contract.

The entry is a named function, not a closure: a closure's input is a type argument of its
call trait, and a capability (the socket a task serves) never is one. A task itself may be
a type argument: it may hold what its input held, but nothing generic can run it.

Rejected for now: model checking (P), temporal proofs, concurrency logics, a stackful
backend, several stores. WASI 0.3's async ABI (a callback returning "wait on this set")
has this design's shape and is the target once the host runs components (docs/decisions.md). Each is a research route, not a step.
