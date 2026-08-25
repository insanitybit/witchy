# RFC-0154 acceptance ledger

This ledger is the durable index for RFC-0154 promotion evidence. Raw timing,
counter, profile, and generated-WAT artifacts remain outside source control.
Every artifact must carry the schema 1 build identity emitted by `bench.sh`.

## Evidence contract

- Fast exploration uses `benchmarks/.build/results-fast.json` or `results-quick.json`.
- Full promotion uses schema 1 artifacts emitted by `bench.sh` with paired
  Witchy/Go kernel samples and warmups.
- Timing artifacts include source and binary identity, toolchains, host,
  optimization configuration, source digests, output checksums, raw samples,
  median, range, MAD, and a deterministic paired bootstrap 95 percent interval.
- Counter evidence is accepted only when three repetitions agree exactly.
- Adversarial fixtures prove call order, effects, allocations, reads, writes,
  traps, and proper tail behavior are preserved across all proposed rewrites.
- Whole-corpus emitted Wasm grows by less than 8 percent and compiler time by
  less than 5 percent.
- A queue row is complete only after its terminal `merged` journal event and
  landed commit are recorded below.

## Track ledger

| Track | State | Baseline artifact | Profile and counters | Focused checks | Workloads / Targets | Branch | Terminal queue evidence |
|---|---|---|---|---|---|---|---|
| 0 baseline freeze & ledger | queued | `benchmarks/.build/results-fast-fib-closure_calls-expr_eval-binary_trees-record_build-word_count-dict_count.json` | Baseline master timing across target and protected suites | Correctness sweep across call-heavy kernels | `fib`, `closure_calls`; protected medians frozen | `docs/rfc0154-phase0-ledger` | pending queue |
| 1 call-site summary & analysis model | planned | call graph analysis descriptors | Structural properties, purity/once classification, frequency hints | `cargo test -p witchy-wir` | `CallSiteSummary` on all WIR call edges; legality guards | `impl/rfc0154-call-site-summary` | pending queue |
| 2 exact callable devirtualization & scalar captures | planned | indirect dispatch transforms | Direct call transformation counters; capture allocation reduction | `cargo test -p witchy-wir` | Exact single-target closure devirtualization; capture scalar replacement | `impl/rfc0154-devirtualize-scalar-captures` | pending queue |
| 3 ownership envelope elision & result scalarization | planned | layout adapter transforms | Forwarding adapter elimination; projection scalarization | `cargo test -p witchy-wir` | Representation adapter removal; multi-value result lanes; root preservation | `impl/rfc0154-envelope-elision` | pending queue |
| 4 fact-specialized cloning & budgeted direct inlining | planned | specialization descriptors | Clones created/shared; inline accept/reject reason counters | `cargo test -p witchy-wir` | Constant/range/callable fact cloning; budgeted direct inlining | `impl/rfc0154-fact-specialization-inlining` | pending queue |
| 5 recursive SCC expansion | planned | recursive expansion model | Recursive edge expansion counters; module code-size budget | `cargo test -p witchy-wir` | Depth-1 and depth-2 scalar recursive SCC expansion; asymptotic stack preservation | `impl/rfc0154-recursive-expansion` | pending queue |
| 6 generalization matrix & promotion closeout | planned | full promotion artifact | 12-pair promotion artifact; generalization matrix verification | Full compiler gate, benchmark suite | Target benchmarks <= 1.00x Go; protected benchmarks within 5% limit | `docs/rfc0154-ledger-closeout` | pending queue |

## Phase 0 Master Baseline (Paired Samples)

Measured on baseline master `4923baafbbe15738d51d9cddc415a759f4a317ac`:

| Benchmark | Witchy Kernel (ms) | Go Kernel (ms) | Ratio vs Go | Output Status |
|---|---:|---:|---:|---|
| `fib` | 31.4 | 20.2 | 1.55x | OK (matches Go) |
| `closure_calls` | 3.8 | 2.2 | 1.72x | OK (matches Go) |
| `collatz` (protected) | 197.2 | 138.8 | 1.42x | OK |
| `loop_sum` (protected) | 25.0 | 25.1 | 1.00x | OK |
| `mandelbrot` (protected) | 34.8 | 34.0 | 1.02x | OK |
| `list_sum` (protected) | 7.9 | 9.1 | 0.86x | OK |
| `dict_count` (protected) | 23.1 | 34.4 | 0.67x | OK |
| `binary_trees` (protected) | 57.5 | 93.4 | 0.62x | OK |
| `word_count` (protected) | 22.9 | 50.2 | 0.46x | OK |
| `regex-redux` (protected) | 11.9 | 14.9 | 0.80x | OK |
| `expr_eval` (protected) | 11.3 | 40.0 | 0.28x | OK |
| `nsieve` (protected) | 3.8 | 3.1 | 1.21x | OK |
| `fannkuch` (protected) | 157.0 | 130.4 | 1.20x | OK |
| `knucleotide` (protected) | 16.1 | 15.4 | 1.04x | OK |
| `record_build` (protected) | 1.5 | 1.9 | 0.77x | OK |
| `list_index` (protected) | 3.0 | 2.7 | 1.11x | OK |

## Pre-Existing Non-Benchmark Consumers

1. **`examples/brainfuck/src/brainfuck.witchy`**: Interpreter loop, instruction dispatch, and command evaluation.
2. **`projects/glamour/src/highlight.witchy`**: Parser combinator callbacks, syntax rule evaluation, and span tagging.
3. **`std/cbor.witchy`**: Item visitor decoding, field extractors, and recursive CBOR container parsing.

## Workload Generalization Matrix (Required Families)

| Family | Positive Case (Specialized / Devirtualized / Expanded) | Near-Miss Case (Preserved Indirect / Materialized) | Primary Metrics |
|---|---|---|---|
| Higher-Order Traversal | Capturing closure callback over list with scalarized environment | Escaping closure stored to heap or returned from function | Direct call opcode, Zero capture allocation |
| Parser Combinator | Exact closure combinator chain with direct dispatch | Dynamic combinator table with opaque runtime choice | Flattened call edge, Traps preserved |
| Policy Predicate | Pure callable value with immutable configuration arguments | Impure predicate with host side effects or mutable state | Hoisted environment, Branch folding |
| Record Visitor | Nominal input/result record projected locally to fields | Result record escaping module boundary or stored to dynamic slot | Scalarized result lanes, Zero adapter overhead |
| Recursive Tree | Non-tail fold or tree walk with scalar leaves | Recursive walk across suspension or dynamic capability boundary | Bounded stack depth, Direct recursive call |
| Numeric Recursion | Pure scalar self-recursive SCC within module budget | Mutual recursion with unknown call targets or host effects | Inlined recursion step, Asymptotic stack parity |
| Escape Control | Callable value invoked within dominating scope | Callable returned from function or passed to unknown dynamic sink | Verified indirect dispatch fallback |
