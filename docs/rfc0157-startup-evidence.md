# RFC-0157 startup evidence

This is the post-simplification baseline required by RFC-0157. It is evidence
for deciding whether a future AOT proposal is warranted; it does not propose
AOT and does not treat the result as a cross-machine performance guarantee.

## Measurement identity

- Implementation commit: `9e143bfe7` (`impl: adopt trusted-publisher executable state boundary`)
- Repository checkout: Witchy worktree `impl/rfc0157-trusted-publisher`
- Host: Apple Mac16,1, arm64, 10 logical CPUs, 24 GiB RAM
- Kernel: Darwin 25.3.0, `xnu-12377.91.3~2/RELEASE_ARM64_T8132`
- Binary: `examples/minigrep/target/release/minigrep`, built by the commit above with
  `target/debug/witchy --release build --target trusted-publisher examples/minigrep`
- Operation: launch the packaged application to search `data/poem.txt`; application
  output was redirected to `/dev/null`.

## Cold-state definition and command

Each sample used a newly created, empty `XDG_CACHE_HOME` directory. There was no
warmup. The packaged executable, linker, and source checkout were already built;
the measured interval includes process startup, launcher envelope validation,
Wasmtime engine/module setup, and guest execution. It does not include publisher
build time. `/usr/bin/time -p` supplied wall-clock `real` time.

```sh
for i in {1..10}; do
    cache_dir="$(mktemp -d /private/tmp/rfc0157-startup-cache.XXXXXX)"
    /usr/bin/time -p env XDG_CACHE_HOME="$cache_dir" \
        ./minigrep/target/release/minigrep nobody data/poem.txt >/dev/null
done
```

## Raw wall-clock samples

In sample order, `real` seconds were:

```text
0.09  0.09  0.10  0.08  0.08  0.09  0.08  0.09  0.08  0.10
```

The median was **0.09 s**. The minimum and maximum were **0.08 s** and
**0.10 s**; median absolute deviation was **0.01 s**. These samples establish
the ordinary cache-enabled trusted-publisher baseline after RFC-0157's
simplification. A future AOT proposal must repeat this matched measurement
with its exact commit, hardware, cold-state definition, raw samples, median,
and dispersion, and demonstrate a material remaining problem first.
