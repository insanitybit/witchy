# witchy performance baseline

Wall-clock mean (ms) per run, lower is better. witchy-wasm is the
compiled backend (WAT -> wasmtime), measured end-to-end including its
~5 ms per-run compile step. `vs go` is witchy-wasm / go (lower is better;
< 1.00 means witchy beats Go). Regenerate with `./run.sh`.

| benchmark | witchy-wasm (ms) | go (ms) | vs go |
|-----------|-----------------:|--------:|------:|
| fib | 63.7 | 35.8 | 1.78x |
| loop_sum | 58.7 | 41.7 | 1.41x |
| collatz | 236.3 | 164.1 | 1.44x |
| mandelbrot | 75.3 | 48.7 | 1.55x |
