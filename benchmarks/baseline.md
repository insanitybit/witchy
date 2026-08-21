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
| fib | 32.5 | 20.5 | 1.58x | 97.9 | 26.7 |
| loop_sum | 51.6 | 25.8 | 2.00x | 112.6 | 40.5 |
| collatz | 211.4 | 143.0 | 1.48x | 284.8 | 152.2 |
| mandelbrot | 36.1 | 35.2 | 1.03x | 103.4 | 42.4 |
| closure_calls | 3.8 | 2.2 | 1.72x | 67.3 | 8.4 |
| list_sum | 9.2 | 10.2 | 0.90x | 83.7 | 17.8 |
| dict_count | 25.4 | 34.0 | 0.75x | 89.9 | 42.1 |
| binary_trees | 67.7 | 93.3 | 0.73x | 133.4 | 105.6 |
| word_count | 143.2 | 51.4 | 2.79x | 239.9 | 59.8 |
| expr_eval | 12.9 | 46.1 | 0.28x | 75.8 | 58.9 |
| nsieve | 8.6 | 3.1 | 2.80x | 98.7 | 11.3 |
| fannkuch | 448.8 | 133.3 | 3.37x | 590.5 | 142.9 |
| knucleotide | 58.5 | 19.7 | 2.98x | 191.0 | 31.2 |
| record_build | 1.4 | 2.1 | 0.68x | 62.6 | 9.5 |
| chan_throughput | — | — | — | 103.0 | 8.8 |
| select_fanin | 24.7 | 0.1 | 269.35x | 528.0 | 4.9 |
| list_index | 5.1 | 2.6 | 1.96x | 76.1 | 11.9 |
