# witchy-native vs Go — user-CPU-time benchmark (load-robust)

Ratio = native / go user CPU seconds; **< 1.00 means witchy-native is faster**.
User CPU time is stable under machine load (unlike wall clock), so these
numbers are trustworthy even on a contended machine. Regenerate with
`benchmarks/cputime.sh`. Outputs are asserted equal before timing.

| benchmark | native (s) | go (s) | ratio | faster |
|---|---|---|---|---|
| fib | 1.62 | 2.11 | 0.77x | witchy |
| loop_sum | 0.36 | 9.86 | 0.04x | witchy |
| collatz | 2.02 | 3.01 | 0.67x | witchy |
| mandelbrot | 2.11 | 2.18 | 0.97x | witchy |
| closure_calls | 1.01 | 1.19 | 0.85x | witchy |
| list_sum | 1.00 | 2.08 | 0.48x | witchy |
| dict_count | 2.52 | 13.50 | 0.19x | witchy |
| binary_trees | 2.06 | 3.14 | 0.66x | witchy |
| word_count | 2.22 | 3.37 | 0.66x | witchy |
| expr_eval | 1.94 | 5.47 | 0.35x | witchy |
