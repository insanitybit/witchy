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
| fib | 32.0 | 20.4 | 1.57x | 55.8 | 36.5 |
| loop_sum | 50.2 | 24.5 | 2.05x | 73.4 | 40.4 |
| collatz | 200.1 | 150.2 | 1.33x | 227.2 | 169.6 |
| mandelbrot | 35.0 | 32.1 | 1.09x | 63.1 | 46.4 |
| closure_calls | 3.7 | 2.3 | 1.66x | 17.5 | 6.7 |
| list_sum | 8.6 | 9.2 | 0.94x | 29.9 | 21.6 |
| dict_count | 26.0 | 33.8 | 0.77x | 48.1 | 50.9 |
| binary_trees | 64.3 | 102.0 | 0.63x | 91.2 | 119.8 |
| word_count | 83.5 | 50.5 | 1.65x | 110.2 | 67.2 |
| expr_eval | 12.4 | 45.9 | 0.27x | 32.7 | 63.2 |
| nsieve | 10.9 | 3.1 | 3.51x | 31.9 | 8.0 |
| fannkuch | 388.7 | 135.5 | 2.87x | 413.3 | 156.3 |
| knucleotide | 34.1 | 13.6 | 2.51x | 61.2 | 27.8 |
| record_build | 8.9 | 7.6 | 1.18x | 30.2 | 19.9 |
| chan_throughput | — | — | — | 158.6 | 7.6 |
| list_index | 5.1 | 2.7 | 1.93x | 22.5 | 7.4 |
