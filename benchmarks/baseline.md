# witchy performance baseline

Wall-clock mean (ms) per run, lower is better. **witchy-native** is the
native backend (witchy -> Rust -> rustc/LLVM); **witchy-wasm** is the
compiled backend (WAT -> wasmtime, with an on-disk compile cache). Both
are measured as prebuilt binaries, like Go. `vs go` is witchy-native / go
(lower is better; **< 1.00 means witchy beats Go**). Collection benchmarks
(list/dict) skip wasm — its immutable `push`/dict ops are O(n^2) at these
sizes. Regenerate with `./run.sh`.

| benchmark | witchy-native (ms) | witchy-wasm (ms) | go (ms) | vs go |
|-----------|-------------------:|-----------------:|--------:|------:|
| fib | 29.0 | 47.6 | 27.4 | 1.06x |
| loop_sum | 4.2 | 36.9 | 41.2 | 0.10x |
| collatz | 167.6 | 395.4 | 226.1 | 0.74x |
| mandelbrot | 47.9 | 61.1 | 49.2 | 0.97x |
| list_sum | 11.7 | — | 16.1 | 0.72x |
| dict_count | 11.4 | — | 45.7 | 0.25x |
| binary_trees | 274.4 | — | 251.4 | 1.09x |
