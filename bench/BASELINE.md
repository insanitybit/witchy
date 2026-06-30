# Baseline — 2026-06-11 (Apple Silicon, Go 1.25.3, wasmtime 45 Speed tier)

Process-level wall times via hyperfine (3 warmups). The witchy leg is the
compiled WASM tier (`witchy sandbox`, compilation cache warm), so its ~4–8 ms
floor includes CLI start + cached-artifact load — visible at these tiny
workload sizes.

| bench | Go | witchy (WASM) | ratio |
|---|---|---|---|
| cpu (4M mixed int ops) | 9.2 ms | 11.5 ms | Go 1.24× (within noise; user-time compute is at parity) |
| listbuild (300k append + fold) | 5.1 ms | 8.3 ms | Go 1.63× (startup-dominated; was an OOM TRAP before in-place push) |
| strings (20k naive appends) | 38.6 ms | 6.8 ms | **witchy 5.7× faster** — the linear-update rewrite makes the naive form builder-class; Go's naive `+=` stays O(n²) |
| hello (cold start) | 8.2 ms | 8.5 ms | parity |

No .NET toolchain on this machine; C# legs run automatically when `dotnet`
is present.

Reading: compute and startup are already in Go's class; the headline win is
the memory-model work (a workload that *trapped* now ties, and the string
builder beats Go's compiler). Next levers per spec/performance.md: arena
reset points (long-running loops), wasm-opt post-pass, threaded actors.

## After Phase 1 (same day)

| bench | Go | witchy (WASM) | ratio |
|---|---|---|---|
| listbuild | 9.8 ms | 9.7 ms | **parity (1.01×)** — was an OOM trap at baseline |
| strings | 41.0 ms | 10.4 ms | **witchy 3.9× faster** |

Memory model landed: in-place push/append/insert (linear-update over shadow
capacity locals), arena watermark resets for escape-free loops (200k-iteration
/ 6 GB-churn soak in constant memory). Remaining levers: dict hash index
(lookup is still a linear scan), wasm-opt post-pass, threaded actors.

## After the dict hash index (same day)

| workload | before | after |
|---|---|---|
| 50k int-keyed inserts | 1.63 s (linear scans) | **10 ms** |
| 20k string-keyed build + lookups | trap-prone via copying path | **17 ms wall** |

With this, every core structure (list, string, dict) builds and reads at
Go-class speed on the WASM tier. wasm-opt measured as not-a-lever at current
codegen quality; parallel actor drain available opt-in.

## Multi-core (RFC-0032) — `vm.par_map` across worker VMs

A CPU-heavy pure function mapped over 16 elements, run in parallel. The witchy
leg uses `vm.par_map`, which dispatches the elements to OS-thread worker VMs (one
wasmtime instance per core, zero-authority); the Go leg uses goroutines + a
`WaitGroup`. 10-core machine, compiled WASM tier, hyperfine.

| bench | Go (goroutines) | witchy (`vm.par_map`) | ratio |
|---|---|---|---|
| parmap | 85.1 ms | 106.6 ms | **0.80× (Go 1.25× faster)** |

Both saturate the cores (user time ~508 ms Go vs ~543 ms witchy). witchy's real
multi-core is within 25% of Go's goroutines on CPU-bound parallel work; the gap is
per-call worker-VM instantiation overhead (a future instance-pool lever). This is
the first witchy workload to use more than one core — parity-preserving, because a
pure function collected by input index yields the same result sequentially.
