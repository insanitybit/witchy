# Binaryen trust and release pin

Witchy's optional post-encoding optimizer is Binaryen `wasm-opt` version 131.
It remains in the generated-code trust boundary because it runs after SafeWIR
ghost facts are erased. `tools/binaryen-version_131.lock` records the reviewed
upstream release tag, source commit, official archive URLs, and archive SHA-256
values for supported Linux and macOS release targets.
The upstream source of record is the
[Binaryen version 131 release](https://github.com/WebAssembly/binaryen/releases/tag/version_131).

Release packaging must verify the selected archive digest, extract it without
changing the `bin/` and `lib/` relationship, and supply the platform's exact
file digests while building Witchy:

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

These names are read with `option_env!`, so a release artifact embeds its tool
path and expected bundle identity. Developer builds with no compile-time pin
may supply the same complete platform-specific set at launch. Compile-time and
runtime values are never mixed. A partial set, relative path, digest mismatch,
unsupported platform, or version output other than exactly
`wasm-opt version 131 (version_131)` disables Binaryen and preserves the
already validated input module. Ambient `PATH` is never consulted.

The current release workflow does not yet install or configure this bundle, so
ordinary release artifacts continue to take the validated-input fallback. This
slice supplies the authenticated runtime mechanism and reproducible identity;
release-workflow integration remains required before RFC-0162 row 13 can be
complete. A compile-time absolute path must name the deployed bundle at runtime;
otherwise packaging must leave the compile-time pin unset and launch with the
complete runtime set.

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
resolution, cache lookup, or invocation. RFC-0162's differential corpus must
exercise this exact staged executable and recipe; the pin does not by itself
establish semantic equivalence.
