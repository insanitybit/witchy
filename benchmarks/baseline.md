# witchy performance baseline

Wall-clock mean (ms) per run, lower is better. witchy-wasm is the
compiled backend (WAT -> wasmtime), measured end-to-end. Cranelift
compilation is cached on disk across runs, so a warm run (what the
harness measures) pays only the frontend + execution, like Go's
prebuilt binary. `vs go` is witchy-wasm / go (lower is better; < 1.00
means witchy beats Go). Regenerate with `./run.sh`.

| benchmark | witchy-wasm (ms) | go (ms) | vs go |
|-----------|-----------------:|--------:|------:|
| fib | 58.7 | 49.5 | 1.19x |
| loop_sum | 38.2 | 34.8 | 1.10x |
| collatz | 213.6 | 171.9 | 1.24x |
| mandelbrot | 53.1 | 47.2 | 1.12x |
