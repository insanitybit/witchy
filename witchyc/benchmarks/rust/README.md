# Benchmarks Game Rust baselines

Source snapshots from the Benchmarks Game, under the [3-Clause BSD license](LICENSE).

| Workload | Rust submission | Local input |
|---|---|---:|
| fannkuch-redux | [Rust #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/fannkuchredux-rust-2.html) | 10 |
| fasta | [Rust #3](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/fasta-rust-3.html) | 250000 |
| mandel | [Rust #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/mandelbrot-rust-1.html) | 2000 |
| nbody | [Rust #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/nbody-rust-1.html) | 1000000 |
| spectral | [Rust #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/spectralnorm-rust-1.html) | 1000 |
| binary-trees | [Rust #5](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/binarytrees-rust-5.html) | 16 |
| k-nucleotide | [Rust #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/knucleotide-rust-1.html) | FASTA 1000 |
| pidigits | [Rust #4](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/pidigits-rust-4.html) | 1000 |
| regex-redux | [Rust #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/regexredux-rust-1.html) | FASTA 1000 |
| reverse-complement | [Rust #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-rust-2.html) | FASTA 1000 |

Run from the compiler directory:

```sh
python3 benchmarks/run.py --compiler "$WITCHYC" --target macos-arm64 \
  --rust-cache ~/Library/Caches/witchy-rust-baselines
```

The cache stores the executable, compiler identity and flags, build duration,
runtime samples, and verified output digest. Changing the sample count
reuses the executable. Use a different cache directory for a fresh baseline.

These are local input sizes. The upstream submissions and Witchy ports may use
different algorithms and thread counts. The runner checks stdout byte for byte
before reporting a comparison. FASTA input generation is outside the timed runs.

Before the first offline build of the Cargo-based submissions, fetch the locked
dependencies with `../state/mbx/bin/mbx exec cargo fetch --locked --manifest-path benchmarks/rust/Cargo.toml`.
Pidigits uses system GMP through Rug. On Apple Silicon with Homebrew GMP,
set `CPATH=/opt/homebrew/include` and `LIBRARY_PATH=/opt/homebrew/lib`.
