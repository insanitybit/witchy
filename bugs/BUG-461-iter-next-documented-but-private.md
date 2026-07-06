# BUG-461: iter.next is documented as the pull primitive but declared private

Severity: MED
Status: FIXED
Verification: SOURCE
Component: `std/iter`, generated stdlib docs, RFC-0042 namespace examples, function privacy
Fixed: 2026-07-06 (`cb57943`)

## Resolution

Fixed by `cb57943` (`std: expose iterator pull primitive`).

`iter.next` is now declared `pub fn`, generated stdlib docs include it, and
`example_tests::std_iter_next_is_public_pull_api` verifies that an importing
module can call `iter.next(iter.from_list(...))` on both the interpreter and
compiled WASM backend.

Validation: `./scripts/check.sh` green in
`/Users/cobrien/workspace/witchy-iter-public`:
`1445 passed / 2 skipped`, plus build, clippy, Witchy fmt, and wasm playground
build.

## Problem

`std/iter` describes `Iter(a)` as a thunk that produces a `Step(a)` and tells
users to "Pull it with `next`." RFC-0042 also uses `iter.next(...)` as the
module-qualified example for imported stdlib types. But the implementation
declares `next` as a private function, not `pub fn`.

Today this is masked by BUG-451: function privacy is not enforced for imports, so
user code can still reach private helpers. If BUG-451 is fixed without also
resolving this API mismatch, the advertised iterator pull primitive becomes
unavailable from user modules. That leaves callers with a public representation
they can destructure manually, or the higher-level `split_first` helper whose
contract is similar but not the spelling the docs and RFC use.

This also makes `std/iter` inconsistent with the neighboring lazy substrates:
`std/future` exposes `pub fn poll(...)`, and `std/task` exposes
`pub fn poll(...)`.

## Code Evidence

- `std/iter.witchy:19-25` documents "Pull it with `next`" but declares
  `fn next(it: Iter(a)) -> Step(a)` without `pub`.
- `std/iter.witchy:317-320` exposes `split_first(...)` publicly as a safe
  first/rest pull helper, but that is not the API name used by the type comment.
- `spec/stdlib.md:870` repeats the generated docs: "Pull it with `next`",
  while no `#### fn next(...)` public API entry is generated.
- `rfcs/0042-module-namespaces.md:74-79` demonstrates
  `iter.next(iter.from_list([1]))`.
- `std/future.witchy:22-28` documents and exposes `pub fn poll(...)`.
- `std/task.witchy:52-54` exposes `pub fn poll(...)` for tasks.
- BUG-451 tracks the current privacy-enforcement gap that makes this private
  function reachable by accident.

## Fix Direction

Pick one coherent iterator contract:

- make `next` public and keep `Step`/`Iter` as an explicitly low-level pull API;
  or
- keep `next` private, update `std/iter` comments, generated docs, and RFC/book
  examples to teach `split_first` as the public pull helper.

The first path is probably cleaner if `Step` remains public and RFC-0042 wants
module-qualified constructor examples. The second path is cleaner only if `Iter`
is meant to become opaque enough that callers should not handle `Step` directly.

## Acceptance

- After function privacy is enforced, the documented iterator pull example still
  typechecks.
- Generated stdlib docs either include a public `iter.next` entry or stop telling
  users to pull an iterator with `next`.
- The chosen API is consistent with `future.poll` and `task.poll`, or the docs
  explain why iterators deliberately use `split_first` instead.
