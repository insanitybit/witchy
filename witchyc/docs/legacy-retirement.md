# Legacy architecture retirement

The active compiler uses the Rust frontend and `src/wasm`, emits Wasm, and runs
it through the Wasmtime host. Required source obligations are checked by Lean.
See [current architecture](proof-architecture.md),
[production builds](wasm-production-build.md) and
[proof-required memory](wasm-proof-memory.md).

The inactive native lowering cluster, unregistered native tests and their old
runner/benchmark/documentation cluster were removed after checking the active
module graph, registered tests and current helper/script dependencies. No
compatibility routes or renamed copies of the obsolete implementation were
introduced. Cargo keeps automatic integration-test discovery disabled and names
only `wasm_scalar` and `wasm_cli`; library tests remain enabled.

## Preserved boundaries

- Some old producer and native-routing source islands remain unchanged while
  ownership is unresolved. These include the old `call_program` source family,
  native entry points and `source_files.rs`. They are outside the active module
  graph and do not promise a buildable historical backend.
- The held native entry point still names `scripts/bootstrap-lean.sh`, so that
  script remains. The active isolated process runner, its metadata and guard
  tests also remain together. A partial deletion beneath these consumers would
  break a current or held dependency.
- All active frontend/Wasm code, Wasmtime host, proof templates/session helpers,
  generic libraries, examples and current fixture files remain. Installed
  helper/SDK generations are preserved.
- Source deletion does not release another owner's interface or authorize SDK,
  administrator, cache or unrelated-work changes.

## Remaining project Lean foundation

The obsolete project Lean test/audit targets and native resident-pilot and
measurement experiments have also been retired. They are outside the active
Wasm compiler and current proof-session helper dependency graph. The active
checker generates obligations using `src/wasm` templates and imports the Lean
SDK; it does not import the old project `Witchy` umbrella.

Some held native/source-routing consumers still name project Lean modules.
Their conservative transitive import closure of 165 modules remains unchanged,
including the old audit rules, runtime modules and dependency-graph helpers.
`proof/Witchy.lean` aggregates only those retained roots, and Lake keeps the
`Witchy` library and `witchy-import-graph` executable. This preserves dependency
boundaries while ownership remains unresolved; it does not restore or qualify
the retired native backend. No active proof or audit rule was weakened.

The complete project proof tree and experiment sources before this second
retirement are available at `ea437ef1b5878cb433e747267946db6426efeeb9`:

```sh
git show ea437ef1b5878cb433e747267946db6426efeeb9:proof/Audit.lean
git show ea437ef1b5878cb433e747267946db6426efeeb9:experiments/resident-pilot/README.md
```

## Historical source and evidence

The complete tree before this cleanup is preserved in Git at `0731e426`.
Historical source or a retired guide can be inspected without restoring an old
compiler route:

```sh
git show 0731e426:src/lower/array.rs
git show 0731e426:docs/vec-library.md
```

Raw qualification and measurement evidence is preserved separately. Old native
measurements and broad-suite counts are historical results; they do not describe
the current Wasm compiler or qualify its safety. Retirement changes no kernel,
source/currentness, axiom-audit, ownership, publication or guest resource checks.
