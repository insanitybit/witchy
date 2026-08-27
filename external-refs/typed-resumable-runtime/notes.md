# Typed resumable runtime prior art

Primary sources reviewed for RFC-0161:

- Rust `Future`: <https://doc.rust-lang.org/std/future/trait.Future.html>
- Rust async book: <https://rust-lang.github.io/async-book/>
- Go language specification, select statements:
  <https://go.dev/ref/spec#Select_statements>
- Swift Evolution SE-0296, async/await:
  <https://github.com/swiftlang/swift-evolution/blob/main/proposals/0296-async-await.md>
- Swift Evolution SE-0304, structured concurrency:
  <https://github.com/swiftlang/swift-evolution/blob/main/proposals/0304-structured-concurrency.md>
- .NET `System.Threading.Channels` overview:
  <https://learn.microsoft.com/dotnet/core/extensions/channels>
- Pony tutorial, actors and reference capabilities:
  <https://tutorial.ponylang.io/types-and-capabilities/reference-capabilities.html>
- Hoare, *Communicating Sequential Processes*:
  <https://www.cs.cmu.edu/~crary/819-f09/Hoare78.pdf>

## Adopted ideas

- Rust: lazy resumable computations, explicit readiness, and ownership-aware
  activation. Witchy uses compiler-owned frames, so it does not adopt `Pin`,
  raw wakers, or executor plurality.
- Go: communication operations as select arms, once-only operand evaluation,
  rendezvous and buffered channels, and backpressure. Witchy uses source-order
  deterministic arm priority rather than pseudo-random selection.
- Swift: lexical child-task lifetimes, typed child results, scope exit that
  waits for children, and cancellation as a request. Witchy initially excludes
  detached tasks.
- .NET: typed bounded and unbounded queues, explicit completion, and asynchronous
  readiness. Witchy uses one deterministic scheduler rather than independent
  reader/writer promises.
- Pony: actor isolation built from message passing and strong ownership. Witchy
  keeps actors as a library pattern over its existing qualifiers.
- CSP: rendezvous communication and guarded choice. Witchy adds exact payload
  layouts, structured ownership, and compiler-generated state machines.

## Rejected transfers

- Go's pseudo-random ready-arm selection and ambient goroutine lifetime.
- Rust's user-visible pinning and unsafe raw-waker construction.
- Swift and .NET executor/thread-pool nondeterminism inside the cooperative VM.
- Abrupt cancellation that can abandon Witchy `must` obligations.
- Pony's full reference-capability type system as a replacement for Witchy's
  existing ownership and capability contracts.
