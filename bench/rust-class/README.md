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
bench/rust-class/run.sh --measure
bench/rust-class/run.sh --enforce
```

`--check` compiles both legs, verifies the Rust kernel is scalar-only, and
compares both untimed results with a pinned independent expected value.
`--measure` additionally prints best-of-seven warm
kernel times and Witchy/Rust ratios. `--enforce` applies RFC-0111's completion
thresholds: geometric mean at most 1.25x and no core case above 1.50x.

The regular gate must use `--check` while the RFC is under implementation.
Changing it to `--enforce` is part of RFC closeout and requires a checked-in
reference-machine report. Wall time remains informational; kernel time is the
acceptance measurement.

## Reproducibility

The harness prints the Witchy and Rust versions, host architecture, OS, and the
exact Rust code-generation flags. Each Rust program exposes the measured work as
`witchy_rust_class_kernel`, outside startup and printing. The Witchy programs
bracket the same work with `Clock.now_monotonic()` and print:

```text
result=<integer>
bench_ns=<integer>
```

Every sample must return the same result. A timing run with a wrong result is
discarded as a correctness failure, never as a slow sample.
