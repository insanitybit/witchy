# witchy performance baseline

Two clocks per benchmark. **kernel** is the compute time measured *inside*
the program with a monotonic clock (witchy `now_monotonic`, Go `time.Now`),
excluding process start and wasmtime instantiation — it isolates codegen
quality. **wall** is the end-to-end `witchy sandbox` / Go-binary time
(hyperfine mean), startup included; the witchy wall−kernel gap is the fixed
runtime-startup tax. `vs go` is witchy/go on the kernel clock (lower is
better; **< 1.00 means witchy beats Go**). Regenerate with `./run.sh`.

| benchmark | kernel witchy (ms) | kernel go (ms) | kernel vs go | wall witchy (ms) | wall go (ms) |
|-----------|-------------------:|---------------:|-------------:|-----------------:|-------------:|
| expr_eval | 12.7 | 47.9 | **0.27x** (3.7x faster) | 82.0 | 63.9 |
| binary_trees | 67.4 | 97.8 | **0.69x** (1.45x faster) | 144.2 | 115.6 |
| dict_count | 25.5 | 33.4 | **0.76x** (1.31x faster) | 102.4 | 55.1 |
| record_build | 1.5 | 1.9 | **0.77x** (1.30x faster) | 67.8 | 8.5 |
| list_sum | 8.2 | 10.3 | **0.80x** (1.25x faster) | 86.8 | 32.3 |
| mandelbrot | 36.3 | 34.6 | 1.05x | 114.8 | 52.4 |
| collatz | 204.9 | 145.3 | 1.41x | 288.8 | 165.3 |
| fib | 31.7 | 20.0 | 1.59x | 106.9 | 32.2 |
| closure_calls | 3.8 | 2.2 | 1.72x | 83.1 | 11.9 |
| list_index | 5.1 | 2.6 | 1.98x | 78.0 | 10.5 |
| loop_sum | 50.6 | 25.5 | 1.98x | 121.2 | 34.8 |
| word_count | 139.8 | 51.0 | 2.74x | 235.8 | 73.6 |
| nsieve | 8.4 | 3.0 | 2.77x | 100.6 | 8.3 |
| knucleotide | 53.5 | 17.2 | 3.11x | 155.4 | 48.6 |
| fannkuch | 447.7 | 132.6 | 3.38x | 523.8 | 154.7 |
| chan_throughput | — | — | — | 119.1 | 22.2 |
| select_fanin | 24.9 | 0.1 | 271.91x | 520.3 | 7.3 |
