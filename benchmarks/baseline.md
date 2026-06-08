# witchy performance baseline

Wall-clock mean (ms) per run, lower is better. witchy-wasm is the
compiled backend (WAT -> wasmtime), measured end-to-end including its
~5 ms per-run compile step. `vs go` is witchy-wasm / go (lower is better;
< 1.00 means witchy beats Go). Regenerate with `./run.sh`.

| benchmark | witchy-wasm (ms) | go (ms) | vs go |
|-----------|-----------------:|--------:|------:|
| fib | 48.2 | 33.3 | 1.45x |
| loop_sum | 43.5 | 38.6 | 1.13x |
| collatz | 220.8 | 173.9 | 1.27x |
| mandelbrot | 56.0 | 49.7 | 1.13x |
