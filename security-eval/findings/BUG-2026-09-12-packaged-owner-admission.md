# Packaged source loses sealed owner admission on reload

Status: reproduced; not fixed. RFC-0165 B11 remains incomplete.

## Observed behavior

The source program runs successfully and prints `packaged capture`. Compiling
the same source to a trusted-publisher executable succeeds. Executing that
package fails before the guest entry runs:

```text
trusted-publisher application startup failed: unknown import: `witchy.private.owner::b4041321942` has not been defined
```

This is a functional failure to preserve the admitted artifact's runtime
requirements, not evidence of capability escalation or arbitrary corruption.
Exact preservation of the Wasm bytes does not preserve the sealed owner-bridge
recipes required to instantiate those bytes.

## Reproducer

```witchy
fn main(console: Console):
    let message = "packaged capture"
    let emit = fn():
        console.print(message)
    emit()
```

The temporary project manifest includes:

```toml
[rune]
name = "package_probe"
version = "0.1.0"

[dependencies]

[targets.trusted-publisher]
confine = "best-effort"
```

```sh
CLI=/private/tmp/witchy-rfc0165-reflect-specialization-target/debug/witchy
PROBE=/private/tmp/witchy-package-owner-probe.FwsoMG
"$CLI" "$PROBE/src/main.witchy"
"$CLI" compile "$PROBE/src/main.witchy" --target trusted-publisher \
  --manifest "$PROBE/witchy.toml" --out "$PROBE/app"
"$PROBE/app"
```

Recorded exits: source 0, package 0, packaged launch 1. The probe directory
contains the exact source, manifest, executable, logs, and individual exit files.
The CLI was rebuilt from integration `b7b7153c8` plus pending bare-recipe v2
changes; this program uses the ordinary source compilation path, not a test
driver. The confinement report shows enforced Seatbelt filesystem/TCP policy
and partial syscall filtering before the missing-import failure.

## Traced cause and repair obligation

Independent read-only review traced source compilation's admitted bytes through
`trusted_publisher::package`, package reload, `run_trusted_application`, and raw
`Runtime::spawn`. Reload validates descriptor digests and Wasm structure but
does not retain canonical admission. The resulting unadmitted worker cannot
register the sealed owner bridge that `spawn_admitted` registers from an
`AdmittedCanonicalArtifact`.

Repair must retain or independently re-establish both exact-byte admission and
the checked runtime recipes. Descriptor hashes or custom-section claims cannot
mint that authority. Recompilation from a closed retained source graph, under
authenticated loader ownership and compatible toolchain inputs, followed by
exact comparison with packaged bytes is a candidate repair, not implemented
evidence. Preserve existing grant, argv, confinement, and output semantics.

Required regression: package and run this owner-bearing source independently of
its original source directory; assert output and ownership cleanup. A package
combining different valid source and Wasm, even with recomputed descriptor
hashes, must reject before guest effects. No test has yet established those
post-repair obligations.
