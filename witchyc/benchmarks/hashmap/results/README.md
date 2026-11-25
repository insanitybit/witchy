# Mutable map measurements

The host was an Apple M4 running macOS 27.2.1, build 26B131.
The candidate used the sealed mutable map from f7bc84f7 and the compiler from
854d9912. Each cell gives the median change in end-to-end elapsed time with
its paired 95% sign interval in brackets. Negative values mean faster runs.
The JSONL files retain every sample, workload checksum source digest, compiler
digest and emitted program digest. See the [benchmark harness](../README.md)
for workloads, timing boundaries, commands and the 5% regression threshold.

| Workload | ARM64 vs c662a6a9 | Wasm vs c662a6a9 | ARM64 vs 34414e82 | Wasm vs 34414e82 |
| --- | --- | --- | --- | --- |
| map | +4.9% [-0.7, +8.1] | +1.5% [-1.7, +3.3] | -3.0% [-6.7, +6.6] | +0.0% [-2.5, +3.2] |
| words | -9.3% [-11.6, -7.8] | -2.8% [-3.8, -1.2] | -2.9% [-8.3, +1.2] | -2.7% [-5.5, -1.4] |
| sparse | -4.2% [-4.8, -3.3] | -0.8% [-2.4, +1.6] | -8.3% [-14.5, -0.4] | -3.2% [-6.4, +2.4] |
| dense | -21.0% [-21.5, -20.8] | -15.3% [-17.1, -13.9] | -13.7% [-22.1, -11.4] | -15.9% [-20.3, -14.0] |
| clustered | -66.8% [-67.0, -66.5] | -63.7% [-64.5, -62.9] | -59.5% [-62.7, -57.3] | -60.3% [-62.9, -56.7] |
| same-hash | -6.7% [-7.0, -6.4] | -5.2% [-8.5, -2.2] | -5.5% [-8.7, -3.7] | -5.1% [-7.8, +1.3] |
| churn | -73.9% [-74.1, -73.8] | -60.9% [-63.2, -57.8] | -73.6% [-74.6, -72.6] | -54.3% [-57.6, -49.3] |

The original-default matrix used 81 pairs per workload. The contemporary-default
matrix used 41. Only the ordinary integer workload on ARM64 crossed the 5%
upper bound in either matrix. An additional 121 pairs per baseline used the
same compiler, source and generated binary digests. Against c662a6a9 the
repeat measured −0.7% [−3.1, +1.2] and all 202 pairs measured +0.4%
[−1.0, +2.2]. Against 34414e82 the repeat measured −1.1% [−3.4, +1.1]
and all 162 pairs measured −1.5% [−3.5, +0.5]. These data include the
original samples from both comparisons.

## Native compiler updates

After native constant division lowering landed in f34aeddf, each ARM64
workload ran for another 81 pairs against each baseline. All 14 upper
confidence bounds were below +5%. Against the default at f34aeddf the ordinary
integer workload measured +1.8% [−3.4, +4.0]. The elapsed-time reductions for
dense, clustered and churn workloads were 21.9%, 60.8% and 74.1%.
The subsequent address-reuse change in 3c028d21 produced byte-identical
programs for all seven workloads, for both the default and candidate maps.
The one-pair artifact check records those digests.

## Landing comparison

The 30ee5600 native compiler also returns small records in registers and
inlines additional wrappers. The ARM64 results below include 41 initial pairs
for each case, plus 121 pairs wherever the original interval crossed +5%.
Parentheses give the total pairs. Every upper bound is below +5%.

| Workload | ARM64 vs c662a6a9 | ARM64 vs 30ee5600 |
| --- | --- | --- |
| map | -0.6% [-2.0, +1.2] (162) | +0.4% [-1.9, +2.9] (162) |
| words | -18.8% [-24.2, -12.5] (41) | -5.3% [-10.3, +3.1] (41) |
| sparse | -7.2% [-11.0, -4.3] (162) | -4.8% [-6.8, -2.1] (162) |
| dense | -21.1% [-32.1, -13.8] (41) | -21.1% [-24.1, -17.5] (41) |
| clustered | -68.7% [-70.2, -66.6] (41) | -67.3% [-69.5, -65.6] (41) |
| same-hash | -2.5% [-7.4, +3.9] (41) | -1.1% [-3.5, +3.2] (162) |
| churn | -75.4% [-76.9, -74.0] (41) | -74.7% [-76.3, -71.6] (41) |

After the console API change in 3de97fca and policy refresh in 37496b41,
all seven native benchmark binaries and six Wasm binaries retained their
measured digests. The Wasm word-count program changed. Its 61-pair rerun
measured −3.4% [−5.9, −1.8] against c662a6a9 and −4.6% [−8.9, +1.7]
against the default from 30ee5600. The final candidate passed all 37 affected
source capability policies.
