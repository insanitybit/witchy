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
