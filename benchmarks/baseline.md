# witchy performance baseline

Two clocks per benchmark. **kernel** is the compute time measured *inside*
the program with a monotonic clock (witchy `now_monotonic`, Go `time.Now`),
excluding process start and wasmtime instantiation — it isolates codegen
quality. **wall** is the end-to-end `witchy sandbox` / Go-binary time
(hyperfine mean), startup included; the witchy wall−kernel gap is the fixed
runtime-startup tax. `vs go` is witchy/go on the kernel clock (lower is
better; **< 1.00 means witchy beats Go**). Regenerate with `./run.sh`.

| benchmark | kernel witchy | kernel go | kernel vs go | wall witchy | wall go |
|-----------|--------------:|----------:|------------:|------------:|--------:|
| fib | 32.0 | 20.4 | 1.57x | 105.0 | 32.7 |
| loop_sum | 52.4 | 26.1 | 2.01x | 177.8 | 43.2 |
| collatz | 204.4 | 146.7 | 1.39x | 336.8 | 166.5 |
| mandelbrot | 36.7 | 35.0 | 1.05x | 165.7 | 54.0 |
| closure_calls | 3.8 | 2.2 | 1.71x | 114.7 | 8.2 |
| list_sum | 10.4 | 10.3 | 1.01x | 121.0 | 30.7 |
| dict_count | 25.6 | 32.6 | 0.78x | 143.1 | 76.1 |
| binary_trees | 67.8 | 96.7 | 0.70x | 183.9 | 118.6 |
| word_count | 144.7 | 56.4 | 2.57x | 279.9 | 101.4 |
| expr_eval | 13.9 | 47.2 | 0.29x | 124.6 | 62.8 |
| nsieve | 9.8 | 3.1 | 3.13x | 166.3 | 18.6 |
| fannkuch | 455.1 | 134.6 | 3.38x | 647.3 | 166.4 |
| knucleotide | 60.1 | 20.4 | 2.94x | 190.7 | 39.5 |
| record_build | 1.6 | 2.9 | 0.56x | 126.4 | 10.5 |
| chan_throughput | — | — | — | 146.4 | 4.9 |
| select_fanin | 26.0 | 0.1 | 281.82x | 602.6 | 6.1 |
| list_index | 5.1 | 2.6 | 1.96x | 109.2 | 6.7 |
