# witchy performance baseline

Wall-clock mean (ms) per run, lower is better. **witchy-wasm** is the
compiled tier (WAT -> wasmtime, with an on-disk compile cache),
measured end-to-end via `witchy sandbox`, including process start and
instantiation — like the prebuilt Go binary it races. `vs go` is
witchy-wasm / go (lower is better; **< 1.00 means witchy beats Go**).
Regenerate with `./run.sh`.

| benchmark | witchy-wasm (ms) | go (ms) | vs go |
|-----------|-----------------:|--------:|------:|
| fib | 46.5 | 34.1 | 1.36x |
| loop_sum | 39.5 | 40.0 | 0.99x |
| collatz | 225.8 | 174.1 | 1.30x |
| mandelbrot | 95.3 | 49.1 | 1.94x |
| closure_calls | 13.6 | 5.4 | 2.52x |
| list_sum | 24.9 | 19.9 | 1.25x |
| dict_count | 67.6 | 49.3 | 1.37x |
| binary_trees | 73.5 | 115.0 | 0.64x |
| word_count | 87.9 | 66.0 | 1.33x |
| expr_eval | 26.4 | 63.5 | 0.42x |
