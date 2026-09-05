# BUG: canonical proper tail sites use `return_call`, which the production engine disabled

Status: fixed on `impl/rfc0165-interpreter` (2026-09-05); resource evidence still owed

## Finding

The canonical backend lowers `TailCall`/`VarTailCall` terminators to the Wasm
tail-call proposal (`return_call`, `return_call_indirect`). The production
`Runtime` configured `config.wasm_tail_call(false)`, following RFC-0090's
original plan to implement proper tail calls by loops and dispatchers so the
locked-down feature set need not admit the proposal. Any admitted canonical
program with a proper tail site therefore instantiated with

```text
failed to compile: wasm[0]::function[1]
```

The canonical pipeline tests never observed this because they instantiate with
their own `wasmtime::Config`, not the production runtime's.

## Fix

The production engine enables the tail-call proposal. Frozen-compiler modules
never emit `return_call`, so their behavior is unchanged. RFC-0165 permits
"target tail calls or a semantics-equivalent bounded-stack dispatcher"; the
canonical backend has chosen target tail calls, so the engine must admit them.

## Evidence still owed (B4)

RFC-0090 allows native tail calls to replace the lowerings "only under
differential and resource tests". The 300,000-transition executable probe
against the admitted artifact through `spawn_admitted` remains required
before B4 can move past candidate-green.
