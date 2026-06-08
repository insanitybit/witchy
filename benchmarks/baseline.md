# witchy performance baseline

Wall-clock mean (ms) per run, lower is better. **witchy-native** is the
native backend (witchy -> Rust -> rustc/LLVM); **witchy-wasm** is the
compiled backend (WAT -> wasmtime, with an on-disk compile cache). Both
are measured as prebuilt binaries, like Go. The `vs go` columns are
witchy / go (lower is better; **< 1.00 means witchy beats Go**).
Regenerate with `./run.sh`.

| benchmark | witchy-native (ms) | witchy-wasm (ms) | go (ms) | native vs go | wasm vs go |
|-----------|-------------------:|-----------------:|--------:|-------------:|-----------:|
| fib | 22.9 | 50.8 | 29.5 | 0.78x | 1.72x |
| loop_sum | 7.6 | 42.8 | 37.8 | 0.20x | 1.13x |
| collatz | 120.7 | 217.3 | 172.0 | 0.70x | 1.26x |
| mandelbrot | 48.0 | 53.2 | 62.4 | 0.77x | 0.85x |
