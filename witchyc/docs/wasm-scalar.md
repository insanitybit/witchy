# Direct scalar Wasm pilot

> **Historical.** This page records the first scalar pilot (2026-11) and its limits then;
> most have since been lifted. For what holds now see docs/wasm-memory-sizes.md (limits and
> host flags), docs/wasm-console.md (output), docs/wasm-signed.md and docs/wasm-f64.md
> (signed and floating numbers), docs/wasm-capabilities.md (files, network and the rest) and
> docs/wasm-production-build.md (the host).

`witchyc wasm FILE -o MODULE.wasm` uses the real Witchy lexer and parser,
then a separate private typed scalar IR/checker and a direct binary emitter.
It never calls the historical text-instruction lowerer, Lean, C extraction,
or a Lean interpreter. This is an experimental opt-in path; existing execution
and proof infrastructure remains intact.

Build with your configured MBX executable and separately owned Cargo target/cache, then:

```sh
witchyc wasm examples/wasm-scalar.wy -o scalar.wasm
witchy-wasmtime-host scalar.wasm --output
```

Expected output:

```text
45
6148914691236517205
```

The runtime is `tools/wasmtime-host`, one Rust process over Wasmtime; nothing else is
installed or started. It runs core Wasm, not a Witchy
interpreter. No WASI or filesystem/network capabilities are linked. `--output`
is an explicit host grant; a module cannot grant itself authority.

## Supported source and checks

- `U8`, `U16`, `U32`, `U64`, `Bool`, unit returns, initialized `let`/`var` locals, assignments,
  direct function calls, return, structured `if` and `while`.
- Unsigned `+`, `-`, `*` trap on overflow/underflow; `/`, `%` trap on zero.
  Narrow arithmetic also traps on overflow of its own width. Unsuffixed literals
  take a contextual width; existing values do not implicitly change widths.
  `U8.to_u64`, `U16.to_u64`, `U32.to_u64` explicitly widen values.
  Comparisons are unsigned, and `&&`/`||` short-circuit. Evaluation is left to
  right. Booleans are distinct from integers in the checker.
- `main()` or `main(console: Console<Write>)`, always returning unit. The only
  output operation is `console.print_int(U64)`, emitting decimal digits and LF.
  The capability cannot be copied, shadowed, converted, forwarded or returned.
  It belongs to the invocation, not to a source scalar local. Helpers are pure.
- Every declaration and statement is checked, including unused functions,
  unreachable code and untaken branches. Non-unit functions must return on
  every statically reachable fallthrough path. Locals have lexical scopes;
  use-before-binding and immutable assignment fail.
- Fully initialized copyable records and fixed arrays (1..32 elements, at most
  32 flattened scalar slots). Nested records/arrays, by-value calls and returns,
  record field reads/writes, array construction/repeat and checked array reads.
  Array index is U64; an out-of-bounds access traps. Indexed assignment to a
  mutable local or record field checks the index once before evaluating the RHS;
  the entire RHS is materialized before writing. Nested indexed assignment places
  (`a[i][j]`, `a[i].field`) are rejected; reads may be nested. Record identity is nominal.
  Owning fields, recursive layouts, invariants and zero-sized aggregates are
  rejected, including unused declarations.

Everything else is rejected, including generic types, signed integers, loans,
moves, allocation, unsafe operations, text literals,
contracts, lemmas (even true ones), predicates, proof blocks, assertions,
loop invariants and binding justifications. No obligations are silently dropped.
One scalar descriptor supplies width, range and storage size/alignment metadata;
all unsigned register carriers are i64, independent of storage/address width.
Aggregate descriptors recursively compute aligned offsets, array strides,
slot types and sizes; the same descriptor controls type checking and
scalarization. These storage descriptions are bound in receipts but are not
exercised as linear-memory addresses. Aggregate call/return ABIs use core Wasm
multi-value results (up to 32 scalars). No reference types or GC features are used.
The scalar profile has no owning storage or loans, so ownership/region cleanup is
vacuous. It does **not** implement the general ownership checker or proof bridge.
A future memory backend must use generic layout descriptors and the agreed
checked stack protocol; this profile creates no linear-memory stack at all.

## Host boundary and limits

The bare host makes sandbox claims only, not Witchy proof/admission claims.
It accepts hostile profile-conforming Wasm as well as compiler output. It checks
actual bytes before instantiation; arbitrary metadata never grants authority.

| Dimension | Bound / policy |
| --- | --- |
| Source and module size | 64 KiB each |
| Guest linear memory, tables, globals | Zero; all such sections/imports/exports rejected |
| Entry | Only a defined `run: () -> ()`, no caller-supplied representations |
| Imports | At most `witchy.scalar.v1.print_u64: (i32, i64) -> ()`, with host grant |
| Output | 64 KiB per invocation; decimal U64 plus LF |
| Execution | 1000 ms worker deadline including validation/compilation/startup; API can shorten it |
| Stack | V8 worker `stackSizeMb: 1`; engine stack exhaustion traps |
| Engine heaps | 16 MiB old generation, 4 MiB young generation |
| Module structure | 256 functions, 4096 locals/function including temporaries, 128 block depth, 65536 instructions |
| Compiler structure | 128 selected declarations, 32 scalar parameter slots/function, 256 source local slots/function, 4096 statement/value visits/function |

Core numeric/control instructions are explicitly allowlisted. The engine also
validates the exact Wasm bytes. Start functions, custom sections and extra
exports are rejected. There is no pointer/length interface, guest linear memory,
indirect call, guest callback or same-instance reentry API.

The output adapter checks the invocation's single valid handle (`1`) and copies
scalar text into bounded host-owned capture storage. Handle `1` in two instances
means each one's own grant; a guessed value equal to a valid local handle is not
distinguishable. No persistent/recyclable handles or borrowed views exist yet.
The adapter performs no blocking I/O. Host CLI stdout happens after the worker
ends and is outside the guest deadline. Output is released only when the run
succeeds (`--partial-output` shows it on stderr after a trap); other effects are not
rolled back. Every instance is discarded after one
invocation, including success. No guest unwinding/cleanup runs on failure.

Worker heap limits are **not** total process RSS or native compiler memory
limits. No whole-host memory/availability guarantee is made. The Wasm engine,
Node worker implementation and this adapter remain trusted. In particular, the
small binary profile parser complements rather than replaces engine validation.
See [Node worker resource limits](https://nodejs.org/docs/latest-v22.x/api/worker_threads.html#new-workerfilename-options).

## Focused validation

```sh
# Route this through MBX, with an owned target and no inherited rustc wrapper.
mbx test --offline --test wasm_scalar
mbx test --manifest-path tools/wasmtime-host/Cargo.toml --release --offline
```

The Node tests write fixtures under `target/wasm-tests` by default, overridable
with `WITCHY_WASM_ARTIFACTS`. They invoke the compiler with an empty PATH and
nonexistent Lean/lake paths. Controls include overflow/trap output, short circuit,
unsigned boundaries, infinite loop interruption, recursive stack exhaustion,
output floods, missing grants, bad handles and forbidden module surfaces.

This is a partial first milestone. Linear-memory heap/stack storage, cleanup of
owning values, checked theorem admission, admission-backed bindings/trusted stores,
resource tables, library authority separation and production hosting remain.

The additional example `examples/wasm-aggregates.wy` returns mixed-width nested
records and arrays through direct calls, copies before mutation, and prints
`40`, `8`, `80`, `3`. All values occupy Wasm locals. The engine bounds its own
stack; no linear-memory stack/spills, allocator, owning cleanup or heap sentinel
exists in this profile. Those consensus acceptance cases remain future work.

## Current-source recheck

`wasm-checked FILE -o MODULE.wasm` prints the narrow checker coverage receipt. To run a
program with its claims, compile its current source with a trusted compiler and run the
module that build wrote; nothing stored between the two is trusted. (A JSON build-receipt
tool, `scripts/wasm-artifact.mjs`, did this through Node until 2026-11-16; it was removed
with Node, docs/decisions.md.) Raw modules remain runnable by the runtime without Witchy
claims; the runtime still checks every module against the closed profile.

`examples/wasm-sort.wy` runs an ordinary source sorting algorithm using checked
indexed assignment; it prints `1`, `3`, `7`, `9`, then the unchanged original
array's first value `9`. Sorting is not recognized or specialized by the compiler.

## Inspecting and observing the pipeline

`witchyc wasm-ir FILE` prints the actual private typed/structured IR after
checking every declaration. It is a debug view, not a stable interchange format
or an opcode input path. `witchyc wasm-measure FILE -o MODULE.wasm` emits the
same bytes and reports separate source-read, lexer/parser, static-check,
emission, receipt and atomic-publication durations. Measurements are excluded
from deterministic artifact receipts.

```sh
python3 scripts/measure-wasm-scaling.py --compiler /absolute/path/to/witchyc \
  --runtime /absolute/path/to/witchy-wasmtime-host --out OUTPUT_DIRECTORY
```

Use a new output directory: the tool refuses existing evidence directories.
This makes three small observations per source. Each run uses a fresh worker;
OS and process-wide V8 caches are uncontrolled. The host separates profile
validation, engine module construction, instantiation/adapter setup and guest
execution, plus whole worker lifetime. Module construction includes the engine's
validation/compilation work there; execution can include additional JIT work.
These observations do not establish a performance improvement. Compiler process
startup and filesystem publication are visible separately from checking/emission.
