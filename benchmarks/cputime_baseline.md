# witchy-native vs Go — user-CPU-time benchmark (load-robust)

Ratio = native / go user CPU seconds; **< 1.00 means witchy-native is faster**.
User CPU time is stable under machine load (unlike wall clock), so these
numbers are trustworthy even on a contended machine. Regenerate with
`benchmarks/cputime.sh`. Outputs are asserted equal before timing.

| benchmark | native (s) | go (s) | ratio | faster |
|---|---|---|---|---|
| fib | 1.72 | 2.21 | 0.78x | witchy |
| loop_sum | 0.48 | 11.01 | 0.04x | witchy |
| collatz | 1.93 | 3.02 | 0.64x | witchy |
| mandelbrot | 2.33 | 2.42 | 0.96x | witchy |
| closure_calls | 1.04 | 1.31 | 0.79x | witchy |
| list_sum | 2.43 | 4.99 | 0.49x | witchy |
| dict_count | 2.78 | 14.86 | 0.19x | witchy |
| binary_trees | 2.13 | 3.03 | 0.70x | witchy |
| word_count | 2.36 | 3.58 | 0.66x | witchy |
