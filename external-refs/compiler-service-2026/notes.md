# Compiler service and native-cache prior art

Reviewed 2026-09-26 for [RFC-0172](../../rfcs/0172-protected-compiler-service.md).
These are source notes, not evidence of a Witchy implementation or speedup.

- [Mozilla sccache](https://github.com/mozilla/sccache): a compiler-cache tool
  with a local client/server architecture. A useful precedent for persistent
  coordination, not proof of a separate-identity security boundary or retained
  Witchy typechecker state.
- [systemd execution configuration](https://github.com/systemd/systemd/blob/main/man/systemd.exec.xml):
  `User=`, managed cache/runtime directories, and process hardening provide
  building blocks for an unprivileged Linux service. Directory ownership and
  IPC authentication still require a complete installation design.
- [Wasmtime precompilation](https://docs.wasmtime.dev/examples-pre-compiling-wasm.html):
  compilation can be separated from execution, and precompiled artifacts can
  remove compilation from startup. This does not promise faster Witchy source
  compilation or portable native artifacts across incompatible configurations.
- [Wasmtime Module deserialization safety](https://docs.wasmtime.dev/api/wasmtime/struct.Module.html#method.deserialize):
  native artifacts require trusted provenance; decoding is not a safety check
  for arbitrary bytes. The implementation must consult its pinned version's
  exact contract, including backing-storage lifetime and immutability. Online
  API documentation reviewed here identifies itself as 50.0.0-dev, not proof
  that Witchy's pinned version has every listed API.

Design inference: a protected local service can combine ownership separation,
resident compiler state, and work deduplication. None of these sources establishes
the magnitude of that combination's benefit for Witchy; matched measurements
remain necessary.

## 2026-09-26 addendum: compiler hosts and shared pending work

- [Roslyn compiler server](https://github.com/dotnet/roslyn/blob/main/docs/compilers/Compiler%20Server.md):
  retains a bounded metadata-symbol cache and supports standalone compilation.
  Its documentation identifies compiler JIT amortization as its largest benefit;
  that benefit is not transferable to Witchy's already-native Rust compiler.
- [Kotlin daemon](https://kotlinlang.org/docs/kotlin-daemon.html): keeps the
  compiler/environment warm across requests, with configurable memory and idle
  lifetime. Separate process memory is not separate filesystem ownership.
- [Bazel persistent workers](https://bazel.build/remote/persistent) and
  [worker protocol](https://bazel.build/remote/creating): a compiler can serve
  repeated requests under Bazel's process management and still support one-shot
  execution. Retained ASTs and other caches require input identity and request
  isolation; worker sandboxing alone does not eliminate internal state leakage.
- [BuildBuddy action merging](https://www.buildbuddy.io/blog/action-merging/):
  production in-flight action sharing, distinct from completed-result caching.
  Highlights cancellation ownership, stalled producers blocking many consumers,
  and shared failures. A local service can use the same principles without
  adopting distributed scheduling or speculative duplicate execution.
- [Nix multi-user mode](https://nix.dev/manual/nix/2.34/installation/multi-user):
  store operations are mediated by a daemon, while untrusted builders run under
  distinct build identities. This informs ownership separation, not a decision
  to run Witchy's compiler as root or copy Nix's entire storage/trust model.

RFC-0172's resulting architectural rule: one standalone compiler library with
optional hosting adapters. Warm reuse must not make the system daemon mandatory
or imply that every host supplies the same cache-integrity guarantee.
