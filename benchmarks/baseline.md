# witchy performance baseline

Wall-clock mean (ms) per run, lower is better. **witchy-wasm** is the
compiled tier (WAT -> wasmtime, with an on-disk compile cache),
measured end-to-end via `witchy sandbox`, including process start and
instantiation — like the prebuilt Go binary it races. `vs go` is
witchy-wasm / go (lower is better; **< 1.00 means witchy beats Go**).
Regenerate with `./run.sh`.

| benchmark | witchy-wasm (ms) | go (ms) | vs go |
|-----------|-----------------:|--------:|------:|
| fib | 52.9 | 30.1 | 1.76x |
| loop_sum | 48.7 | 35.0 | 1.39x |
| collatz | 231.1 | 190.8 | 1.21x |
| mandelbrot | 99.7 | 45.8 | 2.18x |
| closure_calls | 21.4 | 6.8 | 3.13x |
| list_sum | 22.5 | 14.9 | 1.51x |
| dict_count | 73.1 | 46.1 | 1.59x |
| binary_trees | 79.9 | 115.6 | 0.69x |
| word_count | 104.2 | 69.7 | 1.49x |
| expr_eval | 28.5 | 61.1 | 0.47x |
| nsieve | 31.1 | 7.1 | 4.39x |
| fannkuch | 391.4 | 140.5 | 2.79x |
| knucleotide | 55.0 | 18.1 | 3.04x |
