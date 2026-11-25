# Ordinary builds and selected toolchain sessions

`witchyc build FILE -o MODULE.wasm` publishes a sandbox-compatible Wasm module.
Without `WITCHY_WASM_READY_BINDING`, required proofs use the existing fresh Lean
checker and full SDK checks before and after the proof. Proof-free programs need
no Lean installation. Configure the fresh checker with `WITCHY_WASM_LEAN_ROOT`.

For a trusted selected generation, build the body-interface helper:

```sh
python3 scripts/build-proof-session-helper.py --body-interface \
  --compiler /absolute/witchyc --sdk /absolute/private-readonly-sdk \
  --out /absolute/new-helper-output
export WITCHY_WASM_READY_BINDING=/absolute/new-helper-output/ready-binding.txt
witchyc build program.wy -o program.wasm
witchy-wasmtime-host program.wasm --output --allocator
```

Only grant `--output` or `--allocator` when the program needs those capabilities.
The host uses a fresh Wasmtime process with no WASI; files, sockets and the rest are
reached only through the grants the run names (`--dir`, `--net`, `--listen`, ...).
Building does not execute the module or grant host capabilities.

The binding is a trusted operator build record, not an artifact-provided setting
or a signature. It binds the helper's exact bytes and source version to the
canonical SDK root used for shared-library linking and its full content identity.
The Rust checker parses the binding record immediately; a malformed or missing
configured record refuses. Actual helper/root checks and full SDK validation
before and after LOAD remain lazy until a proof needs the selected generation.
An invalid selected generation never silently switches to another checker. Only
absence of the configuration chooses fresh checking.

Each helper launch is bracketed by full SDK snapshots that must equal the selected
identity: one just before the launch, one after its LOAD and before anything the
helper replied is used (its output, an error, or refusal evidence). The second is
taken after the helper's first request, so it also covers what the helper read
while answering it. Only a verified helper's output is ever read back as a refusal.
A refused check ends its helper, and the explanation of a refused build probes with
more helpers: those start two at a time beside the refused one or the first probe's,
each still between two snapshots, and one snapshot serves every helper launched or
loaded on its side of it. A green build takes two snapshots; a refused build with its
explanation usually three (it took four to fourteen).

The operator must prepare and retain a private, distinct-inode read-only SDK copy,
close writer descriptors, and promise not to write, chmod, replace or clean it
until all session helpers have exited. The binding does not enforce this promise.
Read-only permissions do not resist a malicious owner, debugger or administrator.
The production receipt explicitly says `verified_immutable:false` and
`sdk_path_currentness:false`. It selects this generation until session exit;
changes to the original installation are selected only in a newly configured
session. No administrator action or protected installation is implied.

## Wasmtime execution host

Build the separate Wasmtime runtime crate with MBX:

```sh
mbx build --manifest-path tools/wasmtime-host/Cargo.toml --release
export WITCHY_WASMTIME_HOST=/absolute/path/to/witchy-wasmtime-host
```

The runtime pins Wasmtime 49.0.3. The native host checks the module against the
strict binary profile itself and then runs it; there is no other launcher. It has no
WASI linker, cache, deserialization of native code or native fallback. Grants remain
explicit caller options. By default each invocation has 1,000,000 fuel, a 1 MiB Wasm
stack, 4 pages (256 KiB) of linear memory over both memories, one instance, no tables
and 64 KiB of output; `--fuel`, `--memory-pages` (up to 32768) and `--output-bytes`
(up to 256 MiB) grant more (docs/wasm-memory-sizes.md). A one-second deadline
(`--deadline-ms`) includes engine compilation. Output is released only on success
(`--partial-output` shows it on stderr after a trap). These are guest/operation bounds,
not an OS address-space isolation claim.
Runtime dependency resolution and real execution must pass before deployment.

## Repeated ordinary builds

```sh
witchyc build program.wy -o program.wasm --watch
```

Watch compares bounded source contents, not timestamps. Each changed source is
parsed, type/ownership checked and proved before atomic publication. A source
change during checking refuses that publication. Ctrl-C/SIGTERM asks the CLI to
finish its bounded current work and close/reap its checker. An error terminates
the session and leaves the previous output intact; fix the source and restart.

The deterministic stdin interface uses the same publishing implementation:

```text
witchyc build-session program.wy -o program.wasm
build edit1 LOWERCASE_SHA256_OF_CURRENT_SOURCE
build edit2 LOWERCASE_SHA256_OF_EDITED_SOURCE
```

Each successful line publishes the module and prints one `witchy.build.v1` JSON
record containing the checker receipt and separate measurements. EOF closes the
checker. Request IDs are at most 64 ASCII letters/digits/underscore/hyphen; each
line is at most 256 bytes. The source SHA must match the file actually checked.

One checker retains at most one fully audited generic body theorem. Every
current body application is kernel typechecked against its current proposition;
all current caller, predecessor and authored/unused obligations remain checked.
Changed/ineligible body plans use the original current proof before any failure.
No kernel, identity or protocol failure is retried through a fallback checker.
There is no disk admission cache or retained current-query environment.

Each helper keeps its existing eight-operation limit, including LOAD, INSTALL
and STOP: at most five proof requests, reserving room for a possible first body
installation before planning each new source. Watch and the line interface
continue across this limit by renewing a healthy exhausted helper before the
next build. Renewal validates the original binding, checks STOP and reaps the old
helper, then starts with empty body/query state. The next required proof performs
the normal full SDK checks before and after LOAD and freshly installs any body
theorem. Thus every sixth proof build pays startup again; the other edits keep
the warm path. Each helper and request remains bounded, while the logical edit
loop has no fixed proof count limit.

Renewal pins the original operator binding for the entire edit loop, including
idle periods between helpers; it never reloads configuration to select another
generation or fallback. Binding changes, failed STOP/cleanup, invalid proofs and
protocol errors still terminate the loop without publishing the failed request.
This is planned budget renewal, not retry after failure. Restart explicitly to
select a different generation. Separate one-shot CLI invocations still pay
startup each time.

Single builds complete STOP and source validation before publishing. Long-lived
sessions validate current idle state and source bytes before each publication.
A later failed request or STOP cannot retroactively revoke an earlier successfully
checked module; the failure is reported and no failed request is published.
The source binding describes the checked snapshot, not immunity to later edits.

## Receipts and artifacts

Selected-generation proofs use `selected_generation_kernel_checks` with schema
`witchy.selected-generation.kernel.v1`; they never impersonate `fresh.v1`.
Receipts bind current source/compiler/helper/model/audit/generated Lean module and
SDK identities and inventory every current theorem and allowed axiom. The actual
body application uses `kernel_typed_body_application`; other goals say
`fresh_kernel_checked`. Timings are separate from the deterministic proof record.
Selected-generation and line-session file builds also bind exact source and
emitted Wasm hashes. Ordinary proof-free builds retain their platform-independent
path; the selected checker and SHA-based session interfaces currently require macOS.

The artifact adapter accepts `--ready-binding ABSOLUTE_RECORD` for build and run
(or the `readyBinding` API option). That path comes only from the trusted caller,
never from packaged metadata. Execution still recompiles the full source and
compares emitted bytes and semantic identities before sandbox execution. Fresh
and retained-body checks can produce different valid Lean modules; the adapter
validates each receipt and compares current propositions and semantic inputs,
without pretending the generated module hashes are equal. A receipt remains an
untrusted description, not standalone proof admission.

Successful production sessions remove their private temporary proof logs after
owned process cleanup. Production helpers inherit the compiler launcher process
group so a launcher timeout can terminate both; ordinary cleanup kills/reaps only
the owned helper child. Diagnostic helpers keep their separate groups. Failed sessions retain bounded diagnostic evidence and
print its location. The diagnostic-only commands keep their existing evidence
behavior and remain separate from production publication.

The runtime is one Rust process, `tools/wasmtime-host`: it reads the module, checks it
against the closed profile (`src/profile.rs`), enforces the deadline and the output
budget, and runs it in Wasmtime. The Node launcher that used to do the profile check and
process I/O was removed on 2026-11-16 (docs/decisions.md). Grants are operator choices on
the command line, never data in the module.
