# witchy performance baseline

Kernel values are medians of raw in-program compute samples. Wall values
are medians of end-to-end hyperfine samples and include process startup.
The machine-readable sibling artifact retains build identity, inputs,
checksums, every sample, variability, and the paired ratio interval.

| benchmark | kernel witchy | kernel go | kernel node | kernel ruby | kernel rust | kernel vs fastest | wall witchy | wall go |
|-----------|--------------:|----------:|------------:|------------:|------------:|-------------:|------------:|--------:|
| fasta | 455.6 | 290.5 | — | — | — | 1.57x (go) | 845.6 | 273.8 |
| fib | 32.7 | 21.2 | — | — | — | 1.55x (go) | 126.6 | 27.9 |
| loop_sum | 26.0 | 26.2 | — | — | — | 0.99x (go) | 153.2 | 33.6 |
| collatz | 215.5 | 148.1 | — | — | — | 1.45x (go) | 347.0 | 157.1 |
| mandelbrot | 36.7 | 36.4 | — | — | — | 1.01x (go) | 164.0 | 51.0 |
| closure_calls | 3.9 | 2.3 | — | — | — | 1.69x (go) | 122.7 | 7.5 |
| list_sum | 9.1 | 10.1 | — | — | — | 0.90x (go) | 131.1 | 19.3 |
| dict_count | 22.1 | 34.6 | — | — | — | 0.64x (go) | 143.9 | 43.1 |
| binary_trees | 56.5 | 97.5 | — | — | — | 0.58x (go) | 189.8 | 112.6 |
| word_count | 24.2 | 56.6 | — | — | — | 0.43x (go) | 143.9 | 63.8 |
| regex-redux | 12.4 | 15.4 | — | — | — | 0.80x (go) | 136.2 | 19.2 |
| expr_eval | 11.9 | 49.4 | — | — | — | 0.24x (go) | 140.7 | 60.2 |
| nsieve | 3.9 | 3.3 | — | — | — | 1.19x (go) | 147.7 | 9.9 |
| fannkuch | 172.0 | 142.2 | — | — | — | 1.21x (go) | 301.5 | 154.2 |
| knucleotide | 21.4 | 17.4 | — | — | — | 1.23x (go) | 178.2 | 23.2 |
| record_build | 1.6 | 2.4 | — | — | — | 0.68x (go) | 134.6 | 5.4 |
| chan_throughput | — | — | — | — | — | — | 159.2 | 5.3 |
| chan_scheduler | 140.2 | 0.2 | — | — | — | 590.52x (go) | 774.9 | 4.4 |
| select_fanin | 1.9 | 0.1 | — | — | — | 19.71x (go) | 803.5 | 14.2 |
| list_index | 3.1 | 2.8 | — | — | — | 1.12x (go) | 89.9 | 3.3 |
| reverse-complement | 103.8 | 11.8 | — | — | — | 8.76x (go) | 271.4 | 43.2 |
