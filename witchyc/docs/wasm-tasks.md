# Tasks

A task runs a function that may pause, on its own stack, until it finishes. Code that pauses
is ordinary sequential code; whatever steps the task decides when it continues. The design
and its reasons are in docs/design-async.md; this is the reference. `import tasks` (and
`import net` for sockets, timers and `Waiting`).

## Making and stepping one

| | |
|---|---|
| `Task.spawn(&allocator, bytes, work, move input)` | A task that will run `work(input)` with a stack of `bytes` (512 to 1 MiB, a multiple of 16). `work` is a named, non-generic function of one owned input with no references, and no `requires`. `Err(input)` when there is no memory. |
| `task.step()` | Runs it until it finishes, `Ok(())`, or pauses, `Err(token)`. Stepping a finished task traps. |
| `task.resume(word)` | `step`, and the pause it continues from returns `word`. |
| `task.cancel()` | Steps it with `CANCELLED` until it finishes: `Ok(what it reported)`, or `Err(token)` if it still waits after being told twice. A task not yet started begins with `CANCELLED` as its first word (`first_word()`). |
| `task.finished()` | Whether it has finished. |
| `task.close()` | Gives back a finished task's memory; closing one that has not finished traps. A task must be closed. |
| `tasks.close_all()` | Closes every task in a `Vec<Task>` (all finished), then the Vec. |

A `Task` may be kept anywhere, a `Vec` included: its block never moves. It is not a
capability, though it holds what its input held; generic code can move one but not run it.

## Inside a task

| | |
|---|---|
| `pause(token)` | Ends this step; the next continues here. |
| `pause_for(token) -> U64` | The same, returning the word the next step hands in (0 from `step`; the ready entry from `Waiting`; `CANCELLED`). |
| `first_word() -> U64` | Before the first pause: the word the first step handed in, `CANCELLED` when the task was cancelled before it began, so its function can give back its input and return. 0 outside a task. A closure that takes over what it needs and gives nothing back checks it itself: cancelled before it began, it discards what it holds and runs nothing (`tasks/cancel-unstarted-closure.wy`). |
| `report(value)` | Hands `value` to the stepping code, which steps the task again at once (`REPORTED + value`). |
| `pause(ALSO + entry)` | "Also wait for `entry`": the stepping code records it and steps the task again at once; a plain token ends the step. |

A function that may pause says so if it is `pub`: `-> T suspends`. `main` never pauses.
`nosuspend:` blocks and `R: Read nosuspend` bounds refuse a pause where one must not happen.
A function with a contract cannot pause or run a task itself.
`nosuspend` means only that: no pause. It does not mean no other code runs meanwhile; stepping
a task (`task.step()`, `join`) runs that task's code there, without pausing the caller.

## Several at once

| | |
|---|---|
| `join(&mut tasks)` | Runs up to 32 tasks inside the running one until all finish. Woken with an entry, it steps only the tasks waiting on it. |
| `first(&mut tasks)` | Runs them until one finishes, `Ok(which)`; then tells the rest to stop (one round) and waits until they have finished. |

Cancelled, both tell every unfinished task, wait for them to finish, and return
`Err(CANCELLED)`. A task that never winds down holds them up until the run's limits end it.

| `race(&allocator, bytes, a, b)` | Runs two closures that take over what they need (`fn() -> (): listen(move reader)`, docs/wasm-closures.md) as tasks until one finishes, `Ok(0)` or `Ok(1)`; the other is told to stop and waited for. `Err(1)`: no memory, and what was not started was discarded. |
| `both(&allocator, bytes, a, b)` | The same until both finish. |
| `TaskSet.new(&allocator, count, bytes)`, `set.add(closure)`, `set.first()` / `set.join()` | Up to 32 closures started together, all or nothing (a 33rd is discarded and the set fails): `add` never hands work back (work that cannot start is discarded), and `first`/`join` then cancel what did start and give `Err(1)` (`tasks/task-set.wy`). |
| `Task.spawn(&allocator, bytes, closure)` | A task running such a closure; `Err(closure)` gives it back unstarted (`closure.discard()`). |

## Sockets, timers, and a set of waiting tasks (`import net`)

| | |
|---|---|
| `socket.read(&mut buffer)`, `socket.write(bytes)` | Pause the task until the socket is ready; `Err(Cancelled)` when cancelled. |
| `clock.sleep(ms)` | Pauses the task for `ms` or more; `Err(Cancelled)` when cancelled. |
| `clock.timer(due_ms)`, `clock.end_timer(entry)` | A wait entry ready once the clock reads `due_ms`; only a clock makes one. |
| `allocator.share()`, `clock.share()`, `rand.share()` | Another handle, for a task's input; adds no authority. |
| `Waiting.new(&allocator, &listener)` / `Waiting.empty(&allocator)` | A set of tasks waiting on sockets and timers, with or without a listener. |
| `waiting.start(&allocator, task)` | Steps a new task until it waits or finishes, and keeps it. |
| `waiting.step_ready(&allocator, &listener)` / `step_ready_net(&allocator, &net)` | Polls once and steps every task that is ready; finished ones are closed. A task that reports 1024 times in one turn yields to the others. |
| `run_task(&allocator, &listener, bytes, closure)` / `run_task_net(&allocator, &net, bytes, closure)` | From code that is not a task (`main`): runs one closure as a task until it finishes, polling meanwhile; `Ok(what it reported, summed)`, `Err(1)` with no memory (`net/run-task.wy`, `packages/httpd-app.wy`). Inside a task it refuses, `Err(2)`, the work discarded (`net/run-task-inside.wy`). |
| `waiting.cancel_all(&allocator)` | Cancels every task (a shutdown); those that finish are closed. |
| `waiting.close()` | Gives back the set; every task must have finished. |

A poll entry naming no socket, listener or timer of the program reads as closed, so one task's
bad token cannot stop the others.

## Examples

- `examples/echo/tasks.wy`: an echo server, one sequential function per connection.
- `tests/fixtures/tasks/`: stepping, `join`, `first`, cancellation, wake words.
- `tests/fixtures/net/`: `join`, `cancel`, `cancel-all`, `sleep`, `timer`, `timeout` (a read with
  a time limit from `first`), `client-tasks`, `fair`.

## Not yet

- Cancelling a task that has not started runs its function; one that checks `first_word()` first
  does nothing else, one that does not runs to its first pause and is told there.
- `concurrently:` and `first:` blocks: `race`, `both` and `TaskSet` do what they would, as calls.
- One store, one thread: tasks interleave, they do not run in parallel.
