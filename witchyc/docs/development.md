# Focused development

Read the repository coordination protocol before shared work. Use an isolated
worktree and owned compiler/runtime target directories; preserve unrelated work,
active interface owners, proof sources, fixtures and installed SDK/helper assets.
Reserve the host before builds, real proofs, execution or measurements. Source
review and passing focused checks are distinct evidence.

## Fast loop

A source file may open with `import "relative/path.wy"` lines, before any
declaration. The compiler includes each imported file once, ahead of its
importer, and checks it exactly like text written in place; an import grants no
trust. Diagnostics name the file, line and column. A file without imports is
compiled byte for byte as before.

```sh
scripts/run-fixtures.py --compiler BIN --runtime HOST [--binding READY_BINDING] \
  tests/fixtures/string/build.wy tests/fixtures/scalar/truncate.wy
```

Each `NAME.wy` is compiled, run in Wasmtime and compared with `NAME.expected`;
every proof obligation must be freshly kernel checked. `NAME.refused.wy` must
fail to compile with each line of its `.expected` in the diagnostics.
Proof-free fixtures take milliseconds and need no Lean. A program that imports
the Vec library currently spends about 2 s in Lean plus about 3 s hashing the
SDK on each one-shot build; a build session pays the hashing once.

One proof request is capped at 65536 bytes by the compiler and the installed
helper, and at 160 units of proof work (`SEARCHES_PER_REQUEST` in
`src/compiler/proof.rs`: each closing tactic a goal's proof runs, and each 160
bytes of statement), so that no request comes near the checker's per-request
deadline. A program whose proofs fit both is sent as one request, byte for byte as
before. A larger program is split into contiguous requests; each is audited
and kernel checked, a healthy helper that reaches its request budget is
replaced, and nothing is published unless every request passes. Split
programs check every body directly and do not use a retained body theorem.
The receipt marks each obligation and audited dependency with its `request`.
A single obligation larger than the cap is refused.

When a proof is refused under a ready binding, the compiler prints which
function failed and why before the `W0402` line: the unproved `ensures` clause
with its Result arm, an unmet callee `requires` at a call, or a safety
condition inside the body. It first tries the individual body conditions if
every postcondition is the literal `true` or there are no postconditions.
Otherwise it checks each `ensures` clause with the others replaced by `true`.
These diagnostic checks run in fresh helpers after refusal. The `W0402` line itself
points at the clause, `return`, loop, call or function responsible, says what
was not proved there, and ends with `; next:` and one thing to do
(`src/compiler/proof_advice.rs`; docs/decisions.md, "Every proof refusal says
where, what and what next").

## Rust and runtime checks

Use MBX. Cargo runs library tests and the `wasm_scalar` and `wasm_cli`
integration targets. The latter includes native executable checks on supported
hosts. The Wasmtime execution helper is a separate crate with its own pinned
dependencies and target directory. Set `WITCHY_NATIVE_AWS_LC_ROOT` to the pinned
provider installation to include the native crypto cases.

```sh
mbx build --offline --target-dir /absolute/owned-compiler-target
mbx test --offline --locked --release --target-dir /absolute/owned-compiler-target
mbx build --manifest-path tools/wasmtime-host/Cargo.toml --release --offline \
  --target-dir /absolute/owned-runtime-target
export WITCHYC=/absolute/owned-compiler-target/debug/witchyc
export WITCHY_WASMTIME_HOST=/absolute/owned-runtime-target/release/witchy-wasmtime-host
mbx test --manifest-path tools/wasmtime-host/Cargo.toml --release --offline \
  --target-dir /absolute/owned-runtime-target
python3 scripts/run-fixtures.py --compiler $WITCHYC --runtime $WITCHY_WASMTIME_HOST \
  --binding READY-BINDING tests/fixtures/*/*.wy
```

Choose affected Rust filters and individual Wasm script selectors for routine
edits. Use the default test profile to check debug behavior. Time the complete
release command after an untimed build when comparing warm suite runs. Engine
controls require the Wasmtime binary. Set
`WITCHY_WASM_TEST_READY_BINDING` for CLI cases that exercise proof admission and
refusal. Leave `WITCHY_WASM_READY_BINDING` unset for the test command so the
scalar suite can check behavior without a proof checker. If the selected helper
needs a separate Lean root, set
`WITCHY_WASM_TEST_LEAN_ROOT` too. Those test variables let the suite pass the
checker only to the CLI cases that need it. Run the separately gated Lean audit
with `WITCHY_WASM_LEAN_ROOT` and an explicit filter.
The audit runs each control in its own Lean process and checks the SDK identity
before and after the group.

```sh
WITCHY_WASM_LEAN_ROOT=/absolute/lean-root \
  mbx test --offline --locked --release --lib -- --ignored --exact \
  compiler::proof::tests::real_audit_rejects_missing_unrelated_sorry_and_new_axiom
```

Check reported skips and actual output when recording execution or proof
validation.

The ready-binding helper currently builds on macOS ARM64. On Linux ARM64,
set `WITCHY_WASM_TEST_LEAN_ROOT` to run the four proof-diagnostic cases with
standalone Lean. Without either test variable they report skips. The
stale-helper session case requires a ready binding because it checks proof
cache reuse. Set `WITCHY_WASM_LEAN_ROOT` for direct standalone builds and for
the independent audit command above.

The [native runtime](../runtime/native/) supplies `libwitchy_runtime.a`,
the Rust archive that `build.rs` embeds in the compiler. The native CLI tests
exercise the linked image and its platform sandbox. The [native verification commands](native-backend.md#native-runtime-sanitizer-build)
cover the separate Miri, fuzz, and Kani harnesses, which compile the runtime
core modules directly from that tree.

On macOS ARM64 with AWS-LC configured, the compiler selects the provider archive
when a runtime call or launch binding needs it. Other programs link with the
provider-free archive. The compiler stores the archives' identical trailing
bytes once, then writes the exact selected archive into the link directory.

## Proof-required memory and publishing

`scripts/test-proof-memory.py` exercises the actual generic library and focused
fixture with real Witchy proofs, invalid/missing claims, Wasmtime execution and
preserved output. It requires explicit compiler, ready binding, runtime, a new
output directory and a selected positive/negative case; inspect its arguments
before scheduling it. [Memory scope](wasm-proof-memory.md) describes the readonly
CopyStorage boundary and unsupported effects. It does not prove the whole Vec.

Proof-free compilation needs no Lean. Configure either the trusted fresh root
or a selected helper binding as described in [production builds](wasm-production-build.md).
The selected path uses an operator-maintained private distinct-inode readonly
SDK; no process may mutate, replace or clean it during the logical session,
including idle periods between renewed helpers.
Permissions do not prove immutability. Invalid bindings, kernel/identity/protocol
failures and source changes refuse without a fallback checker or publication.

`build FILE -o MODULE.wasm` publishes only after checks; `--watch` and
`build-session FILE -o MODULE.wasm` use the same source-bound atomic path.
Healthy exhausted helpers renew before the next request, stop/reap the previous
helper and start with empty retained state. Renewal pins the original binding
and repeats startup checks; it is not retry after failure. Every current proof
obligation remains checked. No logical edit-count ceiling follows from a
per-helper operation budget. Source validity and successful publication must
be reported separately from later shutdown errors.

## Runtime, resources and performance

Building grants no host authority. Wasmtime runs a fresh instance without WASI,
filesystem or network imports; grant output/allocation explicitly. The strict
launcher enforces profile and deadline checks; the helper also bounds fuel,
linear memory, stack and output. The [production guide](wasm-production-build.md)
records exact limits and the trusted checker/layout/allocator/emitter boundary.
Current guest limits are 1,000,000 fuel, a 1 MiB Wasm stack, at most 196608 bytes
of linear memory and 64 KiB output, with a one-second launcher deadline and
a two-second native watchdog. These are operation bounds, not OS address-space
isolation. Node is launcher/preflight tooling, not the guest execution engine.

Use existing current harnesses only with an exclusive reservation, frozen
source/compiler/tool bindings, new external evidence directories, actual disk
capacity checks and owned-child cleanup. Keep setup, proof-free, valid proof-edit
and rejection timing separate; profile/SDK hashing must not overlap another
measurement. [Performance evidence](wasm-performance.md) and [scaling methodology](wasm-proof-scaling.md)
state measured workloads and limitations. This guide adds no performance claim.

The native-era isolated test runner (`scripts/test-isolated.py`, its inventories and
its tests) is removed (2026-11-17): it was not the Wasm acceptance matrix, which is the
fixture suite and the unit tests. What the measurement scripts used of it, digests and
process-group commands, is `scripts/harness.py`. The obsolete direct-unit/native
benchmark scripts are retired, with no compatibility shims. See
[retirement scope](legacy-retirement.md). Earlier instructions remain accessible
with `git show 0731e426:docs/development.md`.
