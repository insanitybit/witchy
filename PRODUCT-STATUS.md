# Witchy product status

This document separates Witchy's dependable public-preview path from the larger
research and development surface. It is a product-claim boundary, not a feature
freeze: experimental work may continue and may merge without becoming part of
the supported story automatically.

Evidence snapshot: `ce13254592d6b41821d64ae2c6fe9e5bd0ca7b17`.
The snapshot records current evidence; it is not a compatibility promise.
`Supported preview` identifies the intended dependable component contract; it
does not by itself declare the whole project ready for broad public presentation.
The repository-wide gaps at the end of this document still need to close.

## Maturity labels

- **Supported preview** — useful as part of Witchy's primary user journey,
  documented, and covered by executable positive and negative evidence. The
  behavior may still change before 1.0.
- **Experimental** — implemented and available for exploration, but incomplete,
  insufficiently curated, or outside the primary evidence path. It may break or
  change without notice and must not carry a broad product or security claim.
- **Proposed / internal** — design work, implementation infrastructure, or an
  agent/developer tool. It is not a user-facing Witchy capability.

These labels describe the promise, not the amount of code. A heavily tested
component may remain experimental until its user journey and limitations are
coherent. An experimental subsystem may be used by a supported path only where
that exact boundary fails closed and has dedicated evidence.

## The supported-preview journey

The primary Witchy story is one end-to-end capability workflow:

1. Write and type-check an ordinary statically typed Witchy program.
2. Inspect and diff the host authority its `main` function requires.
3. Run it with explicitly bounded authority.
4. Observe missing grants and confinement escapes fail loudly.
5. Compile the same program to portable WebAssembly and run it in the sandbox.
6. Use `parity` to compare the interpreter oracle with compiled-WASM behavior.
7. For a deliberately trusted application, build a self-contained
   `trusted-exe` with checked root bindings.

This journey is the front-page contract. Package distribution, a hosted
registry, browser frameworks, and proposed language mechanisms are interesting
adjacent work, not prerequisites for it.

## CLI maturity matrix

| Surface | Maturity | Executable evidence and boundary |
|---|---|---|
| `witchy --version` | Supported preview | Packaging and installed-archive smoke require the version and exact source commit. |
| `witchy <file.witchy>` | Supported preview | Runnable examples and installed-archive smoke exercise direct source execution and non-zero failures. |
| `witchy check` | Supported preview | The release smoke accepts valid source and rejects invalid source; compiler tests cover diagnostics and backend acceptance. |
| `witchy fmt` / `fmt --check` | Supported preview | Formatter round-trip tests cover repository sources and documentation examples; installed smoke proves check, rejection, rewrite, and re-check. |
| `witchy test` | Supported preview | Plain tests have zero real authority; deterministic fixture plans run through shared interpreter/Wasmtime state machines with transcript parity and JSON CI evidence; explicit owned integration tests may receive Dir/Net grants. Dependency escalation, malformed plans, backend divergence, and CLI outcomes have focused coverage. |
| `witchy caps`, `caps-diff`, `grants-check` | Supported preview | Capability tests and executable examples cover footprint reporting, widening rejection, rights precision, malformed grants, and over-request rejection. These tools report demanded authority; they do not prove that trusted host/runtime code is defect-free. |
| `witchy sandbox` | Supported preview | E2E tests cover explicit Dir/File/Net/Secret grants, deny-by-omission, precompiled WASM, malformed inputs, and escape rejection. The documented filesystem symlink race remains a limitation. |
| `witchy compile --target wasm` | Supported preview | Installed smoke compiles a fresh program to portable WASM and runs the resulting bytes through the same distributed sandbox host. |
| `witchy emit-wasm` | Supported preview | Alternate direct spelling for emitting the portable binary artifact. It shares the compiler and precompiled-sandbox evidence with `compile --target wasm`; the duplicate spelling is not a separate product surface. |
| `witchy parity` | Supported preview | The fail-closed example sweep, diagnostic parity tests, and seeded-divergence positive control prove that the checker detects observable backend disagreement. It is a compiler-verification command, not an application workflow step. |
| `witchy emit-wat` | Supported preview | It renders the same module used by the compiled sandbox path and is covered by parity-sensitive compiler tests. It is an inspection tool, not a separate backend. |
| Single-rune `new`, `init`, `build`, and `run` | Supported preview | Hermetic project tests and installed smoke create, build, run, and test temporary local projects without reading compiler inputs from the checkout. Multi-rune dependency behavior is classified separately. |
| `witchy --release build --target trusted-exe` | Supported preview | RFC-0092 tests and installed smoke cover checked bindings, source deletion, empty-`PATH` execution, argv preservation, Dir confinement, and corruption rejection. Running the result is a whole-artifact trust decision, not sandboxing of its trusted root. |
| `witchy expand`, `which`, and `doc` | Experimental | Implemented and tested as authoring/inspection tools, but not required by the primary journey and not yet subjected to a curated first-user workflow. |
| `witchy pm` and multi-rune/path/registry `add`, `build`, `run`, `update`, `audit`, `tree`, `outdated`, `why`, `why-cap`, `verify`, and `vendor` | Experimental | Substantial hermetic and E2E coverage exists, but dependency identity, update atomicity, build metadata, and documentation remain a broader evolving contract. These commands must not be used to justify the supported core. |
| `build-step` and package build-capability workflows | Experimental | The confined BuildOut path has tests, but cache, generated-source, dependency, and capability-widening behavior are still an evolving package-system boundary. |
| `publish`, `promote`, `yank`, and registry `list` | Experimental | These are Coven lifecycle operations. Their existence does not establish a supported hosted registry or trusted-publishing service. |
| `coven-serve` | Experimental | The embedded registry server is dogfood for Witchy and the package protocol. It carries no availability, durability, hosted-service, or independent-security-review promise. |
| `witchy lsp` and editor integrations | Experimental | Available for development, but completeness, syntax freshness, and first-user editor setup are not part of the supported-preview acceptance path. |
| `witchy stats` | Proposed / internal | Deterministic compiler-optimization counters used for implementation evidence. It is intentionally outside normal help and is not an application profiling promise. |
| `coven-gen-issuer`, `coven-mint-token`, and `coven-issuer-jwks` | Proposed / internal | Test-only identity-provider and key tooling. These commands do not constitute a supported authentication or deployment workflow. |

## Language and subsystem maturity

| Surface | Maturity | Product boundary |
|---|---|---|
| Core values, functions, records/ADTs, pattern matching, errors, modules, generics, traits, and `let`/`var`/`own` conventions | Supported preview | Exercised across the runnable book, examples, formatter round trips, type-checking tests, and interpreter/WASM differential tests. Compatibility remains intentionally unstable before 1.0. |
| Typed host capabilities, rights narrowing, footprint inspection, and explicit sandbox grants | Supported preview | This is Witchy's defining product surface. The guarantee is confined to the documented compiler/runtime/host trust boundary; it is not a claim of general memory safety or independent security review. |
| Interpreter oracle and compiled-WASM backend agreement | Supported preview | Observable success, output, exit behavior, and diagnostics are differential-gated, with a positive control proving the gate can fail. |
| Portable WASM guest execution | Supported preview | The separately installed Witchy host remains trusted; the guest receives only explicitly bound host capabilities. |
| `trusted-exe` | Supported preview | Self-contained trusted application artifact with checked embedded bindings and corruption detection. Embedded digests do not authenticate a publisher. |
| Standard-library APIs used by the supported journey and curated examples | Supported preview | Documentation freshness and executable examples cover this subset. This status does not silently promote every std module. |
| Bounded async tasks, typed channels, and structured concurrency ([RFC-0129](rfcs/0129-concurrency-tasks-and-channels.md)) | Supported preview | Deterministic scheduling, typed carriers, structured task obligations, backend parity, one-time resumable state, and the sustained compiled memory gates are covered by the RFC acceptance matrix. This promotes bounded workflows and measured carrier shapes; it does not promise that a logically unbounded channel provides infinite storage. |
| Parallel worker VMs ([RFC-0129](rfcs/0129-concurrency-tasks-and-channels.md)) | Experimental | Core destination. True parallelism is distinct from deterministic cooperative tasks and crosses owned serialization and capability boundaries. Its cost and availability need a curated workflow separate from `chan` examples. |
| Generators and lazy iterators ([RFC-0130](rfcs/0130-generators-and-iterators.md)) | Supported preview | `Iter`, `gen fn`, `yield`, adapters, collection, diagnostics, tooling, and the installed examples are covered by the RFC acceptance matrix. Every accepted suspension shape resumes an owned frame with one-time effects and bounded live state; a residual nested yielding CFG fails at its source declaration instead of selecting replay semantics. |
| Static reflection ([RFC-0131](rfcs/0131-reflection-and-comptime.md)) | Experimental | Core destination. `Reflect`, `Mirror`, derives, structural JSON encoding, docs, and backend parity are implemented. It is retained language surface awaiting an installed curated workflow and limitation review. |
| `comptime`, typed quotation, derives, and tagged literals ([RFC-0131](rfcs/0131-reflection-and-comptime.md)) | Experimental | Core destination. Structured syntax, hygiene, provenance, deterministic zero-capability evaluation, expansion tooling, and generated-code checking are implemented. Raw source compatibility paths still need an explicit public policy. |
| `region:` ([RFC-0128](rfcs/0128-regions-and-reclamation.md)) | Experimental | Core destination. Lexical regions, conservative copy-out, Wasm reclamation, counters, docs, and parity are implemented. Promotion promises those semantics, not future destination inference. |
| `mode opt`, explicit references, and ownership/layout contracts ([RFC-0127](rfcs/0127-ownership-and-opt-mode.md)) | Experimental | Core destination. The mode boundary and RFC-0122 reference system are implemented and proven. Promotion requires a curated installed workflow and measured claims for each performance contract; normal mode remains reference-free. |
| Existential trait values and other recently landed semantic extensions | Experimental | Implementation evidence may exist, but a new semantic feature does not become a front-page commitment merely by merging. |
| Runtime `Dynamic` values ([RFC-0132](rfcs/0132-runtime-dynamic.md)) | Experimental | Core destination. Authenticated descriptors, owned payloads, checked decode, public field and registered method access, trait queries, capability accounting, tooling, and backend parity are implemented. The explicit dynamic boundary remains outside the supported preview until its standalone workflow and limitations are curated. |
| Multi-package dependency resolution and remote package lifecycle | Experimental | Local single-rune workflows are supported separately. Remote identity, update, vendor, build-step, and registry behavior form a larger contract. |
| Coven, trusted publishing, and Coven Web | Experimental | No public service, availability promise, independent audit, or supported distribution contract is implied. |
| Glamour, browser applications, playground, and the docs application | Experimental | These provide dogfood and executable documentation evidence. They are not yet promised as a supported general frontend platform. |
| Grimoire/Coven integrated installation (RFC-0095) | Proposed / internal | Not part of the supported preview or `trusted-exe` contract. |
| Unaccepted lexical or other proposed language extensions | Proposed / internal | RFC discussion is not product availability. |

## Promotion rules

An experimental surface becomes supported preview only when all applicable
conditions are met:

1. A useful end-to-end workflow succeeds from a clean installed binary or other
   independently usable artifact.
2. Failure behavior is explicit: invalid inputs, missing authority, corruption,
   and boundary escapes are rejected for the intended reason.
3. Interpreter/WASM parity is executable where both backends apply.
4. The primary documentation and CLI help agree with actual behavior.
5. At least one small, curated example demonstrates why the feature is useful.
6. Known limitations and the trusted boundary are stated without global safety
   language.
7. No known high-severity defect remains inside the promoted contract.
8. A person unfamiliar with the implementation can complete the workflow
   without repository-specific assistance.

Promotion is explicit and tracked in this file. Merging an RFC, test, or large
implementation does not promote a feature by itself. A supported surface may be
demoted if its evidence or documentation stops matching reality.

## Claim discipline

Public claims about Witchy should be one of:

- directly supported by an executable acceptance path;
- narrowly qualified with the relevant trust boundary or limitation;
- labeled experimental; or
- removed from the public front door.

In particular, Witchy should not claim that it is "safe at every level," that a
registry is "truly safe by default," or that test volume constitutes an
independent security audit. The defensible statement is narrower: Witchy makes
host authority explicit in types and artifacts, checks it at defined boundaries,
and has executable evidence for the supported-preview path above.

## Remaining public-credibility work

The classification is necessary but not sufficient. Before presenting the
supported preview broadly, Witchy still needs the following. The five-example
curated path is now documented in [`examples/README.md`](examples/README.md),
separately from the exhaustive development inventory.

- one tracked and internally consistent source of product defects and maturity;
- an independent first-hour trial in which unfamiliar users complete the
  supported journey without author assistance;
- a README and top-level help pass that foregrounds only the supported journey
  and labels adjacent systems consistently; and
- measured compile, link, startup, and representative execution budgets for the
  supported examples.

These tasks improve curation and evidence without preventing experimental
language or ecosystem development.
