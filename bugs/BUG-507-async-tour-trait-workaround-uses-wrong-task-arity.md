# BUG-507: Async tour trait workaround uses wrong `Task` arity

Severity: LOW
Status: FIXED
Verified: 2026-07-09 fixed on master 040ce13b
Component: book async tour, trait async workaround, `std/task`, generated async docs

## Resolution

`book/src/tour-async.md` now says the trait workaround returns `Task(a)`, matching
`std/task`, generated stdlib docs, and the parser diagnostic's `Task(_)` wording.
It also includes a concrete minimal signature:

```witchy
from task import Task

trait Fetcher:
    fn fetch(self, url: String) -> Task(String)
```

Focused verification:

```sh
rg -n 'Task\\([^)]*,[^)]*\\)' book/src/tour-async.md std/task.witchy spec/stdlib.md
```

No matches.

## Problem

Historical problem: the async tour explained the current restriction that `async fn` cannot be a
trait method, then gives the workaround as declaring a plain trait method that
returns `Task(m, a)`.

That type did not exist in the shipped stdlib. The canonical task type is
`Task(a)`: channel message erasure lives inside `Step`/`__Msg`, and typed channel
endpoints carry `Sender(m)` / `Receiver(m)` separately. A reader following the
tour will write a trait signature with the wrong arity before they ever reach the
actual workaround.

This was small, but it sat directly on a release-facing language feature edge:
async methods work for inherent impls, async trait methods do not, and the docs
need to make the supported substitute feel deliberate rather than half-migrated.

## Evidence

- `book/src/tour-async.md:239-243` says a trait that wants an asynchronous
  operation declares a plain `fn ... -> Task(m, a)`.
- `std/task.witchy:37-50` defines `Step(a)` and `Task(a)`, with no second type
  parameter on `Task`.
- `spec/stdlib.md:2388-2410` documents the generated stdlib surface as
  `Task(a)`.
- `crates/witchy-syntax/src/parser.rs:406-416` has the parser diagnostic right:
  trait async/gen methods should be written as plain `fn` returning
  `Iter(_)`/`Task(_)`.

## Why this matters

The project is trying to present async, channels, and traits as a coherent
language story. A stale `Task(m, a)` signature makes the task/channel split look
like an unfinished API migration and sends users toward a type annotation the
compiler cannot accept.

This is distinct from BUG-354, which tracks stale `std/future` docs claiming
language async lowers through `Future`, and BUG-254, which tracks duplicated
`std/task`/`std/chan` implementation bodies. This bug is only the public async
tour's incorrect trait-workaround type shape.

## Expected fix

Change the workaround prose to use `Task(a)` and, ideally, show a tiny concrete
trait/inherent-delegate example:

```witchy
trait Fetcher:
    fn fetch(self, url: String) -> Task(String)
```

The example should either import/use the canonical `task.Task` shape explicitly
or follow the same module import style used elsewhere in the async chapter.

## Acceptance

- `book/src/tour-async.md` no longer contains `Task(m, a)`.
- The async tour's trait workaround names `Task(a)` and aligns with the parser
  diagnostic's `Task(_)` wording.
- Searching book/spec/std docs for `Task(` finds no two-argument task type
  except historical bug/RFC discussion.
