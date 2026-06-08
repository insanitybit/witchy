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
| fib | 19.3 | 37.5 | 32.7 | 0.59x |
| loop_sum | 5.9 | 33.3 | 54.2 | 0.11x |
| collatz | 131.1 | 238.4 | 179.1 | 0.73x |
| mandelbrot | 78.2 | 76.5 | 71.4 | 1.09x |
| list_sum | 32.8 | — | 33.1 | 0.99x |
| dict_count | 17.4 | — | 47.5 | 0.37x |
