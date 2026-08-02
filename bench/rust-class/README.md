# Scalar-only Rust comparison

This is the paired Witchy/Rust evidence harness required by RFC-0111. It uses
the same algorithms and validates results before reporting time. Rust's LLVM
loop and SLP vectorizers are disabled, and `run.sh` requires the measured kernel
symbol and rejects packed-vector instructions anywhere in its Rust translation
unit, including out-of-line helper bodies reached by the kernel.

The corpus covers every benchmark family required by RFC-0111:

- `scalar_int` — mixed integer arithmetic;
- `scalar_float` — scalar floating-point recurrence with an integer checksum;
- `packed_records` — construct and traverse a packed record list; and
- `list_pipeline` — construct, filter/map-equivalent traversal, and reduction;
- `closed_sum` — fixed-layout closed-sum construction and dispatch;
- `generic_helpers` — direct and generic helper boundaries over packed data;
- `destination_record` — repeated unique packed-result construction into a dead destination; and
- `recursive_values` — allocation-heavy construction and traversal of a recursive sum.

Run from anywhere:

```sh
bench/rust-class/run.sh --check
bench/rust-class/run.sh --measure --report /path/to/report.json
bench/rust-class/run.sh --enforce --report /path/to/report.json
bench/rust-class/run.sh --verify-report /path/to/report.json
```

`--check` compiles both legs and compares both untimed results with a pinned
independent expected value. On `arm64`/`aarch64` it also verifies that the Rust
translation unit is scalar-only. The regular merge gate runs this mode against
a Witchy binary authenticated to the candidate commit. On other supported
hosts, including x86 Linux and macOS, it says explicitly that the result is
correctness-only and is not performance evidence.

`--measure` additionally records best-of-seven warm kernel times and
Witchy/Rust ratios. `--enforce` records the same complete report, then fails if
the geometric mean exceeds 1.25x or any core case exceeds 1.50x. Both timed
modes require a clean worktree and an explicit report path. They reject a
caller-supplied `WITCHY` binary and build exact clean `HEAD` in a disposable,
harness-owned Cargo target before authenticating its embedded commit and hash.
They are not part of the regular gate: RFC closeout requires a reviewed report
from a pinned reference machine before timing becomes acceptance evidence.

`--verify-report` performs no builds or benchmarks. It validates the report's
versioned shape and cross-field invariants, including exact case order, clean
commit provenance, the independent ordered result oracle, fixed sample count
and flags, ratio arithmetic, thresholds, and verdict. The normative structural
shape is [`report.schema.json`](report.schema.json); `--verify-report` is the
authority for semantic and cross-field invariants that JSON Schema does not
express.

Scalar certification and reportable `--measure`/`--enforce` runs currently
support only `arm64`/`aarch64`. Those modes fail closed on x86 rather than
treating an incomplete mnemonic denylist as proof that a translation unit is
scalar-only. The source-level
[`rejected-x86-report.json`](fixtures/rejected-x86-report.json) fixture records
that report-verification boundary. The ARM-shaped
[`rejected-result-report.json`](fixtures/rejected-result-report.json) fixture
differs from a valid report only by a one-unit `scalar_int` result mutation and
records the independent-oracle boundary.

## Reproducibility

The harness records the exact Git commit and clean-tree state, authenticated
Witchy version and binary SHA-256, full `rustc -vV` output, CPU model, host
architecture and OS, exact Rust code-generation flags, and fixed sample count.
It deliberately omits timestamps, so identical inputs have a stable report
shape and ordering. Each Rust program exposes the measured work as
`witchy_rust_class_kernel`, outside startup and printing. The Witchy programs
bracket the same work with `Clock.now_monotonic()` and print:

```text
result=<integer>
bench_ns=<integer>
```

Every sample must return the same result. A timing run with a wrong result is
discarded as a correctness failure, never as a slow sample.
