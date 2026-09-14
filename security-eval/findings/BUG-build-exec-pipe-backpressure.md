# Confined executable stdin/stdout backpressure

Status: correction integrated and focused tests passed; merge-queue landing
remains pending in the BuildExec SafeWIR integration slice.

## Defect

The existing Unix paths in `ConfinedFile::run`
(`crates/witchy-runtime/src/confine.rs`) write the entire input into a piped
stdin before `wait_with_output` starts draining piped stdout and stderr.
A tool such as `cat` can fill its stdout pipe and stop reading stdin while the
parent is blocked writing stdin. Neither side can then advance.

This is a pre-existing process-I/O liveness defect, not evidence of forged
capability authority or memory corruption. BuildExec's small stdin echo tests
do not exercise it. The canonical route must not introduce a second process
runner to work around it.

## Focused reproducer

Given an already-granted `ConfinedFile` for `cat`, the following round trip
must complete with exact output. The input exceeds ordinary pipe capacities:

```rust
let input = "x".repeat(1024 * 1024);
let output = cat.run(&[], &input)?;
assert!(output.status.success());
assert_eq!(output.stdout, input.as_bytes());
```

No fresh execution of the old deadlocking path is claimed here.

## Correction obligations

- Feed stdin concurrently with draining stdout and stderr inside the existing
  confined runner, without changing executable selection or process fences.
- Close stdin when feeding completes, preserve I/O-error reporting, and reap
  the child even when writing its input fails.
- Keep any private executable snapshot alive until the child has exited.
- Add the large-input round trip and an error/cleanup regression to the
  focused runtime tests before recording this finding as resolved.

## Focused evidence

`ConfinedFile::run` now feeds stdin on a scoped thread while the existing
collector drains stdout and stderr, joins the writer, and retains executable
storage until child completion. It does not change executable selection or
the process fence.

`confined_cat_roundtrips_input_larger_than_pipe_capacity` and
`confined_stdin_failure_finishes_child_and_preserves_nonzero_output` passed in
native run `fcf8cff4-94cc-4b18-b9f4-202b71ceb334` (84/84 selected tests).
The tests cover a multi-megabyte exact round trip and completion/error cleanup.
Log: `/private/tmp/witchy-build-exec-graph-integrated-focused.log`.
This is focused correctness evidence, not a compiler-wide memory-safety claim.
