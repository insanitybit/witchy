# Scalar callback forwarding

The checked lowerer recognizes a helper whose entire body invokes its first
callback parameter once, forwarding its remaining scalar parameters in order.
A non-tail call with a named, no-capture scalar leaf target lowers directly to
that target. The target has at most four parameters and 32 expression nodes.
No AST is substituted, and the original pointer-keyed type facts stay attached
to their original expressions. Initial checking, final validation, and Wasm
admission still apply.

The helper and target must have matching checked source types and conventions.
Captured or unknown callbacks, additional helper work, escaping callbacks,
recursive targets, ownership transfers, write-back parameters, unsupported
declaration metadata, and proper-tail calls retain their ordinary lowering.
The existing `closure-elide` switch disables the rule for differential checks.

## Reproduction

Save independently built optimized baseline and candidate binaries, then run:

```sh
python3 scripts/scalar-callback-bench.py \
    --baseline /path/to/baseline --candidate /path/to/candidate \
    --output state/callback-samples --rounds 21
```

The harness primes both binaries, alternates order, checks an independently
computed checksum, and retains every observation. It records binary and fixture
digests, optimization settings, and OS identity. `--iterations` changes the
corpus size for both binaries. The loop's Clock interval excludes compiler,
JIT, and fixture setup; whole-process wall time is recorded separately.
The verdict cache is disabled. Current `spawn_admitted` executes exact admitted
bytes without Binaryen or executable-cache lookup, with Cranelift Speed and
inlining enabled.

Use `scripts/compile-bench.sh --binary CANDIDATE --against-binary BASELINE`
for the separate compiler-cost spread. For native-code inspection:

```sh
WITCHY_STD_DIR="$PWD/std" /path/to/compiler compile \
    benchmarks/scalar_callback_forwarder.witchy --out state/callback.wasm
PATH="$PWD/scripts/bin:$PATH" cargo run --release -p witchy-lower \
    --features native-test-runtime --example scalar_callback_codegen -- \
    state/callback.wasm state/callback-native
xcrun llvm-objdump -dr state/callback-native.cwasm
```

The inspection example uses the pinned Wasmtime dependency, enables its
optimizing inliner, and saves both native text and the symbol-bearing object.
Serialized objects are inspection artifacts; this harness never deserializes
or executes them.

## Local evidence, 2026-10-07

Baseline: `bfee28959a38fc4e1504501d110ba372b9e6d36d`. Environment: Darwin
arm64, Rust 1.100.0-beta.3, Wasmtime 49.0.2, and Cranelift 0.136.2. Saved binary
identities and raw observations live in the local evidence directory; these
measurements do not describe general application performance.

| Invocations | Samples per binary | Baseline loop median | Candidate loop median | Speedup |
| --- | --- | --- | --- | --- |
| 200,000 | 9 | 4.204 ms | 0.198 ms | 21.20x |
| 2,000,000 | 21 | 39.645 ms | 1.991 ms | 19.91x |
| 20,000,000 | 9 | 428.736 ms | 20.663 ms | 20.75x |

The two-million-call paired bootstrap 95% interval is 19.51–20.12x. Whole-process
medians are 150.27 ms and 112.94 ms. The tiny corpus is dominated by startup, so
its whole-process medians are approximately equal.

SafeWIR shows two closure/environment allocations and one reference clone
removed per helper invocation. These are structural counts, not allocator
instrumentation. Native disassembly independently shows the baseline loop's
allocator/free-list work and indirect route, while the candidate loop calls the
scalar target directly with no closure allocation. A direct scalar call remains
in native code: eliminating calls alone does not explain the gain.

Wasm shrinks from 42,964 to 42,445 bytes. The main function's native symbol
shrinks from 19,016 to 18,484 bytes, and total defined Wasm function code shrinks
from 22,136 to 21,604 bytes. The page-padded native text segment remains 32,768
bytes on both sides.

Seven alternating compiler samples show baseline/candidate median CPU seconds
of 0.23/0.24 (real), 0.08/0.08 (stdlib), 0.06/0.06 (corpus), and 0.88/0.86
(async). The real-program ranges are identical at 0.21–0.25 seconds, and async
ranges overlap. This establishes no clear compiler-time change.

Three alternating twenty-million-call processes per binary show approximately
77.6 MB median maximum RSS on both sides. This measures the whole compiler/JIT
process, not guest allocation bytes, and establishes no clear RSS change.

Samply records the unsigned binary, but JIT frames remain unsymbolized and
Wasmtime reports that jitdump is unsupported on this platform. Native
disassembly supplies the decisive attribution. Hardware counters and Valgrind
are unavailable here.

Focused coverage includes admitted optimized/unoptimized results, exact direct
target selection, argument invocation count and order, division/remainder and
division-overflow traps, first-argument abort ordering, named shadowing,
unsupported patterns, preserved tail sites, and Int/Bool/Float/zero-argument
signatures. The canonical pipeline integration group passes 18/18. The initial
broad dtest run passes 608/620 and exposes 12 failures in existing documentation,
statistics, browser, async, and older performance cases; that run predates the
enabled recognizer. Landing validation remains separate from these measurements.
