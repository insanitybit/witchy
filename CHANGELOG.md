# Changelog

Witchy follows a deliberately compatibility-unstable 0.x release policy. A
release is a tested checkpoint, not a promise that source, bytecode, package
metadata, or command-line interfaces will remain compatible with the next 0.x
release.

## 0.1.0 — private release candidate

Witchy 0.1.0 is the first installable toolchain checkpoint. Distribution is
private: authorized collaborators download native archives from the private
GitHub repository and verify them against the attached `SHA256SUMS` file.

### Included toolchain surface

- `witchy --version`, with the exact release commit embedded in release builds;
- source checking, canonical formatting, and `fmt --check`;
- direct source and project execution;
- in-language unit tests and explicitly granted integration tests;
- local project scaffolding and builds using the embedded Witchy package-manager
  front end;
- compilation to portable WebAssembly and execution through the native Witchy
  sandbox host;
- capability-footprint inspection and the backend-parity checker;
- the implemented indentation-based, statically typed language surface,
  including records and algebraic data types, traits and generics, ownership
  conventions, capabilities, errors, generators, and cooperative concurrency;
- `witchy --release build --target trusted-exe`, which produces a self-contained
  host-native application executable.

The release gate checks interpreter/compiled-WASM agreement, including complete
runtime diagnostics, across the maintained differential suite. That is evidence
for the exercised programs and properties; it is not a proof that the compiler
has no defects.

### Release artifacts and verified targets

Publication is fail-closed unless the archive for each target is built and then
smoke-tested on a runner with the same native architecture:

- `x86_64-unknown-linux-gnu`;
- `aarch64-apple-darwin`;
- `x86_64-apple-darwin`.

Windows is not a 0.1.0 release target. The release archives are deterministic
given the already-built binary and release inputs; the native compiler binaries
are not claimed to be reproducible builds.

### Portable WebAssembly trust model

A portable `.wasm` file is a guest module, not a standalone native application.
It requires a compatible Witchy host. The host validates the module and grants
only the requested capability imports and explicit launch roots. The module is
not authenticated merely because Witchy can execute it; consumers must establish
the module's provenance separately.

### Trusted executable trust model

`trusted-exe` appends compiled application WASM and a checked capability-binding
plan to the complete Witchy native runtime. The resulting executable needs no
separate Witchy installation or launch-time grant flags. Internal SHA-256 digests
detect corruption and incompatible payloads before `main` executes.

Running one is a whole-artifact trust decision: the user trusts the application,
embedded runtime, and distributor. Capabilities still constrain delegation
inside the application, but they do not sandbox the trusted application root
from the user. The embedded digests detect corruption; they do not authenticate
the publisher. Publisher authentication for 0.1.0 comes from authorized access
to the private GitHub repository and release.

### Known limitations and exclusions

- Compatibility may break without deprecation before 1.0.
- macOS artifacts are neither Apple-notarized nor independently code-signed.
- Current `Dir` confinement rejects lexical, absolute, and existing symlink
  escapes, but its canonicalize-then-open design is not race-free against a
  concurrent local symlink swap.
- Remote Coven registry lifecycle and trusted publishing are not part of the
  installability promise for the 0.1.0 toolchain archive. Local source/project
  workflows are exercised by the installed-artifact smoke test.
- Grimoire/Coven integrated trusted installation is not included. Proposed
  existential, `Dynamic`, lexical-extension, and Grimoire semantics are not
  advertised as 0.1.0 functionality.
- There is no crates.io, npm, Homebrew, public registry, public documentation
  host, or unauthenticated release download for 0.1.0.
