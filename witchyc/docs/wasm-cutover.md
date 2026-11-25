# Active direct Wasm compiler

The active Rust library and CLI use the Witchy lexer/parser, a private typed
copyable-value IR/checker, and direct binary core-Wasm emission. The active
library modules are `ast`, `diagnostic`, `lexer`, `parser`, `token` and `wasm`.
This active-route inventory supersedes historical pilot references to opt-in
compilation or intact legacy execution; the scalar profile details still apply.

```sh
witchyc build examples/wasm-scalar.wy -o scalar.wasm
witchy-wasmtime-host scalar.wasm --output
```

For selected-generation checking, repeated publishing builds and lifecycle semantics,
see [ordinary production builds](wasm-production-build.md).

`build FILE -o MODULE.wasm` is an alias for `wasm FILE -o MODULE.wasm`.
Both write the same Wasm bytes and print no success report. `wasm-checked`
uses the same arguments and prints its checker receipt JSON; `wasm-measure`
prints phase measurement JSON. `wasm-ir FILE` prints the checked debug IR.
`lex FILE` and `syntax FILE` remain frontend diagnostics; parsing alone does not
establish type safety. There is no default output path or implicit execution.

The host's `--output` is an explicit grant. Building a module grants no runtime
authority. The production launcher validates bytes, imports and exports, then
executes one fresh bounded Wasmtime process with no WASI imports or native
fallback. Build/select the native host and see its exact fuel, memory, output
and deadline limits in [ordinary production builds](wasm-production-build.md).
Node supplies profile validation and launcher tooling; guest execution uses Wasmtime only. Wasm sandbox confinement
does not establish per-object memory safety inside linear memory. The trusted
checker and emitter establish the supported Witchy object and frame rules.

## Current language boundary

The executable profile supports unsigned scalar types, Bool, initialized locals,
structured control flow, direct calls, checked arithmetic, and fully initialized
copyable records/fixed arrays, including concrete generic record specializations.
It checks unused selected functions, templates and unreachable statements as
well as executed code. See [generic records](wasm-generic-records.md).

`U8.truncate(value)`, `U16.truncate(value)` and `U32.truncate(value)` narrow a
U64 to its low bits without trapping; `to_u64` widens. Opening `import "path.wy"`
lines bundle other source files, each once, as described in
[development](development.md).

Concrete copyable `Result<T,E>` values support private calls, constructors and
exhaustive `match`. Every arm is checked, even for a known constructor. Only the
active payload becomes a source value; inactive ABI carriers convey no value or
ownership. That was the first slice; sums are now declared types too, may hold
owners and other sums, are stored in buffers, and are matched through shared
references and with nested patterns. See [sum types](wasm-sums.md).

Direct calls can temporarily borrow copyable scalar, record and fixed-array
places, including concrete generic records and nested field/index projections.
Shared and unique permissions are checked across argument evaluation and nested
calls, conservatively treating each source root as one loan domain. Addressed
values use initialized cells from the common layout descriptor in private,
bounded frames; padding is preserved. Full frame checks precede stores, indices
are checked before address conversion and writes, and normal returns restore
the stack pointer. References cannot be stored in fields or buffers; a function may
return a shared reference derived from a shared-reference parameter
([references](wasm-references.md)). See
[aggregate borrowing](wasm-aggregate-borrows.md), [frame safety](wasm-frames.md)
and [the frame host](wasm-frame-host.md).

Checked affine `Buffer<T>` values and one-owner records support explicit moves,
MUST cleanup, call-scoped storage loans and typed Init/read/take/replace operations.
The allocator has a fixed 96 KiB heap and 64 metadata slots; it does not grow Wasm
memory. Physical identities, bounds, slot state, codec and counts are checked by
trusted generated helpers. See [ownership](wasm-linear-buffer.md),
[typed storage](wasm-typed-storage.md) and [generic functions](wasm-generic-functions.md).
Ordinary library source implements [Vec growth](wasm-vec-growth.md) and
[UTF-8 String](wasm-string.md) over copyable storage elements.

Selected scalar lemmas, bounded U64 source contracts, generic Init wrappers and
one-owner record contracts use fresh Lean kernel/type/axiom audits. Actual caller
preconditions are checked, including unused and unreachable selected occurrences.
The contracted library push-spare slice has one universal body theorem and one
allocation-derived generic caller-site theorem reused by U8, U64, Bool and a padded
record. See [source contracts](wasm-source-contracts.md),
[storage contracts](wasm-storage-contracts.md), [body proofs](wasm-vec-body-proof.md)
and [caller proofs](wasm-vec-caller-proof.md). Default Vec/String source is not
silently annotated with contracts; the spare-slice proof does not prove growth.

All source/type/ownership checks finish before Lean. Zero obligations resolve no
Lean installation. Authored functional claims cannot be replaced by runtime safety
checks or a true unrelated lemma. Zero-sized storage and persistent references
remain outside the language; loop and growth contracts, owner elements and an opt-in
proof cache have since been added (`docs/progress.md` is the current account). Absence of a proof block grants no unsupported
authority. There is no C/native fallback, Lean extraction or Lean execution backend.

The compiler/type/ownership checker, VC translation, allocator/codec/layout and
Wasm emitter remain trusted and tested. Kernel checking establishes generated
logical propositions, not a whole-compiler, physical heap refinement or termination
proof. Artifacts freshly recheck packaged source and current compiler/tool bindings;
they are not reusable Lean admissions or an installed trusted store.

## Retired active routes and retained sources

The historical lowerer, source composition, proof-generation/build drivers and
suite server are absent from the active library module graph. The CLI rejects
`lower`, `lower-hints`, `lower-proofs`, the suite-server routes, and old native
build options such as `--library`, `--loan-limit`, `--executable-calls`,
`--proof-debug` and `--fresh-verification`. Invalid arguments are rejected before
opening an input or output, preserving an existing destination.

Unclaimed historical lowering, native test, tooling and guide clusters have
been retired. Some uncompiled source islands remain while ownership is
unresolved; existing proof sources and active helper assets are preserved.
[Retirement scope and historical recovery](legacy-retirement.md) describe this
boundary. Cargo disables automatic integration
test discovery and explicitly registers `wasm_scalar` and `wasm_cli`; library
unit tests remain active. Further Wasm tests can be registered when their source
exists. Use MBX and a separately owned target/cache for focused Rust validation.
The JavaScript host/artifact tests are separate focused checks; artifact execution
requires the built Wasmtime host. No full historical
suite is implied by either result.

### Cooperating compiler clients

A launcher can start `witchyc compiler-session PRIVATE_SOCKET` in a private directory
(mode 0700) and keep its stdin open. The compiler announces `WITCHY_COMPILER_SESSION_1`
on stdout. Each connection carries one request. The client sends a big-endian u32 field
count followed by UTF-8 fields, each prefixed with its big-endian u32 byte length. The
response is a big-endian u32 exit status followed by four length-prefixed fields for
stdout, stderr, a grant-review ticket and diagnostic counters. EOF on the owner's stdin
drains the accepted work and removes the socket. Every build finishes its own proof checker
before publishing its output. Wait for the compiler's
exit status. Select a new compiler process for a different executable or environment.

Clients may send `wasm-grants INPUT DOCUMENT`, `wasm-caps` queries or
`build INPUT OUTPUT HEAP_PAGES SLOTS STACK_BYTES`. Use absolute paths. For
`wasm-caps --by-file INPUT CWD`, the last field is the request's report directory.
For a reviewed executable, send `prepare-grants INPUT DOCUMENT DOCUMENT_TEXT`, apply
the product's launch rules to the returned flags, then send
`build-checked TICKET INPUT OUTPUT` or `release TICKET`. A build consumes its ticket.
Source, imports, grant document, policy and checker selection are checked at publication.
A client that needs cancellation-safe delivery can stage the result in its own private
directory and rename it after receiving success. Coven and Grimoire ship a Python client
for this protocol. Existing single-command entry points continue to work.
