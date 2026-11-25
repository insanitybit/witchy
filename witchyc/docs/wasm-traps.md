# How a program can stop: `wasm-traps`

`wasm-caps` says what a program can do; `wasm-traps` says how it can fail. For each function
`main` reaches, it lists what can stop the run with a trap there, from the program as it is
emitted (a trait method is the implementation that runs), and through which calls:

```
$ witchyc wasm-traps tests/fixtures/tasks/interleave.wy
main: task misuse; overflow through count
Task.close: task misuse
Task.step: task misuse
count: overflow
1 reachable functions never trap
```

A trap is the program's fault: an operation with no answer (`overflow`, `division by zero`,
`index out of range`), `unreachable` reached, `task misuse`, or a `host refusal` of what the program passed (taking a socket from an empty group slot, waiting on more entries than the host allows) (stepping a finished task,
closing one that has not finished, a stack size out of range). Division by a nonzero number
written as such is not listed. Running out of stack, fuel or time is a limit of the run, not
a fault of the program, and is not listed; nor is a host refusing a handle, which typed code
never forges.

It is a report, not a check. A function whose scalar proof shows every unsigned `+ - *` of
its body in range is emitted without those checks, and lists no overflow (scalar_vc.rs records
which; docs/decisions.md). `nopanic fn` is checked against this: it and what it calls must have no way to trap
(`tests/fixtures/nopanic/`).
