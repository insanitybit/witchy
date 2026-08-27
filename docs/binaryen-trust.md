# Binaryen trust and release pin

Witchy's optional post-encoding optimizer is Binaryen `wasm-opt` version 131.
It remains in the generated-code trust boundary because it runs after SafeWIR
ghost facts are erased. `tools/binaryen-version_131.lock` records the reviewed
upstream release tag, source commit, official archive URLs, and archive SHA-256
values for supported Linux and macOS release targets.
The upstream source of record is the
[Binaryen version 131 release](https://github.com/WebAssembly/binaryen/releases/tag/version_131).

`scripts/pinned-binaryen.py install` maps a supported Rust target to one lock
asset, downloads (or accepts) only that archive, verifies its digest before
extraction, extracts only the exact executable and required runtime library,
verifies both file identities and the exact version output, and emits absolute
GitHub step outputs/environment values. It never adds Binaryen to `PATH`.
Developer and corpus builds consume that complete platform-specific set:

```sh
WITCHY_PINNED_WASM_OPT_PATH=/absolute/release/toolchain/binaryen/bin/wasm-opt \
WITCHY_PINNED_WASM_OPT_SHA256=<sha256-of-exact-wasm-opt> \
cargo build --release
```

The macOS release archives dynamically load `lib/libbinaryen.dylib` through an
`@loader_path/../lib` rpath. Their builds additionally set:

```sh
WITCHY_PINNED_BINARYEN_LIB_SHA256=<sha256-of-exact-libbinaryen.dylib>
```

The Linux v131 release executables are static PIEs and do not accept a runtime
library digest. The lock file records every archive, executable, and required
runtime-library digest used by this policy. The runtime embeds those reviewed
file digests and rejects a configured digest that is not the lock's value for
the current platform; a caller cannot bless an arbitrary same-version binary.

These names are read with `option_env!`; developer builds with no compile-time
pin may supply the same complete platform-specific set at launch. Compile-time
and runtime values are never mixed. A partial set, relative path, digest mismatch,
unsupported platform, or version output other than exactly
`wasm-opt version 131 (version_131)` disables Binaryen and preserves the
already validated input module. Ambient `PATH` is never consulted.

The release workflow runs the exact differential corpus using the installer's
absolute authenticated path, then clears that build-time path and builds with
`WITCHY_BUNDLED_BINARYEN=1`. `release-package.sh` verifies and packages the
same bundle at `libexec/witchy-binaryen/`, plus a copy of the identity lock.
The installed binary resolves only
`libexec/witchy-binaryen/bin/wasm-opt` relative to its own `bin/witchy` path and
uses the compiled reviewed digests. Runtime pin variables cannot redirect a
bundled release. `release-smoke.sh` checks the exact archive members and
digests, poisons the runtime pin variables, puts a rejecting fake `wasm-opt` on
`PATH`, and requires authenticated Binaryen evidence from direct
installed-source execution. Installations that copy only `bin/witchy` (including
the current `cargo-binstall` metadata) intentionally have no bundle and take the
validated-input fallback unless strict evidence mode is requested. The release
archives are the distribution form that supplies production Binaryen.

The runtime reads and hashes the configured executable and, on macOS, its
required sibling library. It copies those exact bytes into an owner-only,
bundle-digest-addressed `bin/` and `lib/` staging tree, rechecks the staged
files before every invocation, and executes only the staged path with an empty
environment. The authenticated bundle identity, exact version output, actual
argument vector, recipe schema, and input Wasm all participate in optimized
cache identity. Optimizer input and output live in a private temporary
directory. Every cache hit still enters Wasmtime through `Module::new`
validation. Unexpected optimizer failure or invalid output uses the validated
unprocessed module and does not populate the optimized cache.

Optional fallback is observable with `WIRDIAG=1`, which reports
`WIRBAIL wasm-opt-unavailable: <reason>`. Evidence runs use
`WITCHY_REQUIRE_PINNED_BINARYEN=1`; if the exact reviewed tool cannot be
authenticated, module construction fails instead of silently comparing the
unoptimized module twice. The requirement may be embedded at build time or set
at launch. Required mode bypasses optimized-Wasm cache reads, reauthenticates
and invokes the tool for each module on the cacheable production engine, and
rejects process failure or invalid output. The non-cacheable preempt engine
never uses Binaryen. Explicitly disabling the `wasm-opt` optimization still
disables it.

After Binaryen output passes Wasmtime validation, `WIRDIAG=1` emits one
`WIRTRACE binaryen-applied` record. It names the release, exact version-output
digest, recipe schema and argument vector, bundle/executable/library digests,
and exact input/output module digests. The row-13 corpus parses and independently
checks this record for every post-pipeline execution; its pre-pipeline lane
requires that no such record exists. A separate `WIRTRACE binaryen-input`
record binds the exact pre-lane module digest to the post-lane optimizer input,
so output agreement cannot hide different internal pipelines.

The production recipe is:

```text
-O4
--all-features
--inlining-optimizing
--inline-functions-with-loops
--flexible-inline-max-function-size=100
--one-caller-inline-max-function-size=200
```

Modules carrying the `PreserveRaw` optimizer policy return before Binaryen
resolution, cache lookup, or invocation. `tests/binaryen_production_differential.rs`
executes every applicable SEC fixture and eight deterministic generated
adversarial modules before and after this exact production recipe. Both lanes
must independently match the case's expected success/output or runtime-failure
class; pre/post agreement alone is not accepted as an oracle. The pin and corpus
provide bounded evidence, not universal semantic equivalence.
