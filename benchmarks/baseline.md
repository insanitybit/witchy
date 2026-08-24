# witchy performance baseline

Kernel values are medians of raw in-program compute samples. Wall values
are medians of end-to-end hyperfine samples and include process startup.
The machine-readable sibling artifact retains build identity, inputs,
checksums, every sample, variability, and the paired ratio interval.

| benchmark | kernel witchy | kernel go | kernel node | kernel ruby | kernel rust | kernel vs fastest | wall witchy | wall go |
|-----------|--------------:|----------:|------------:|------------:|------------:|-------------:|------------:|--------:|
| fasta | 381.0 | 274.9 | — | — | — | 1.39x (go) | 750.7 | 290.0 |
| fib | 31.4 | 20.2 | — | — | — | 1.56x (go) | 130.0 | 20.8 |
| loop_sum | 25.0 | 25.1 | — | — | — | 1.00x (go) | 155.8 | 32.0 |
| collatz | 197.2 | 138.8 | — | — | — | 1.42x (go) | 326.8 | 152.0 |
| mandelbrot | 34.8 | 34.0 | — | — | — | 1.02x (go) | 166.5 | 40.0 |
| closure_calls | 3.8 | 2.2 | — | — | — | 1.72x (go) | 117.5 | 4.3 |
| list_sum | 7.9 | 9.1 | — | — | — | 0.86x (go) | 122.2 | 12.1 |
| dict_count | 23.1 | 34.4 | — | — | — | 0.67x (go) | 135.6 | 38.6 |
| binary_trees | 57.5 | 93.4 | — | — | — | 0.62x (go) | 163.6 | 98.5 |
| word_count | 22.9 | 50.2 | — | — | — | 0.46x (go) | 141.9 | 56.4 |
| regex-redux | 11.9 | 14.9 | — | — | — | 0.80x (go) | 136.9 | 26.5 |
| expr_eval | 11.3 | 40.0 | — | — | — | 0.28x (go) | 130.7 | 54.8 |
| nsieve | 3.8 | 3.1 | — | — | — | 1.21x (go) | 209.7 | 14.6 |
| fannkuch | 157.0 | 130.4 | — | — | — | 1.20x (go) | 290.9 | 145.9 |
| knucleotide | 16.1 | 15.4 | — | — | — | 1.04x (go) | 144.2 | 21.7 |
| record_build | 1.5 | 1.9 | — | — | — | 0.77x (go) | 121.9 | 5.3 |
| chan_throughput | — | — | — | — | — | — | 159.1 | 5.3 |
| chan_scheduler | 142.0 | 0.2 | — | — | — | 585.79x (go) | 782.9 | 4.1 |
| select_fanin | 2.0 | 0.1 | — | — | — | 21.00x (go) | 677.2 | 0.0 |
| list_index | 3.0 | 2.7 | — | — | — | 1.11x (go) | 89.9 | 7.9 |
| reverse-complement | 106.7 | 12.2 | — | — | — | 8.78x (go) | 290.5 | 43.1 |
