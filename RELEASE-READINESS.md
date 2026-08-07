# Witchy 0.1.0 release readiness

This is the executable-evidence ledger for the 0.1.0 release. RFC status is not
acceptance evidence. `PASS` requires a command or workflow result on the
recorded release commit; `EXCLUDED` is deliberately outside the release
contract.

**Status: SHIPPED.** Witchy 0.1.0 is publicly released at
<https://github.com/insanitybit/witchy/releases/tag/v0.1.0> (tag `v0.1.0`,
commit `80110cfc06f2347484328e7e043735e49aa47b8d`). The release carries exactly
four assets: prebuilt archives for `x86_64-unknown-linux-gnu`,
`x86_64-apple-darwin`, and `aarch64-apple-darwin`, plus a `SHA256SUMS`
manifest. The uploaded `SHA256SUMS` on the release page is the authoritative
checksum record.

## How the release was produced

[`.github/workflows/release.yml`](.github/workflows/release.yml) is the
release pipeline, generalized to any annotated `vX.Y.Z` tag:

- A tag push runs the full gate on the exact tagged commit: workflow security
  audit (zizmor), build + clippy bug-lints + `cargo nextest` (unit,
  integration, e2e), the checked-heap differential fuzzer, the backend parity
  sweep over every runnable example, the from-scratch whole-system acceptance,
  `witchy fmt --check`, and the browser-compiler / runnable-book / docs
  bundle.
- Each target is built on a native runner (with a runtime proof that the
  runner matches the advertised target), packaged reproducibly (pinned
  `SOURCE_DATE_EPOCH`, canonical `release-package.sh` /
  `release-checksums.sh` logic), and smoked via `release-smoke.sh`
  (installed-archive plus trusted-exe checks) before upload.
- A manifest job requires the exact three-target asset set and generates the
  combined `SHA256SUMS`.
- The publish job creates an immutable **draft**; going live requires a
  separate explicit `workflow_dispatch` with `action=publish`, which verifies
  the existing draft's assets are byte-identical before flipping it public.
  `action=verify-candidate` / `action=verify-draft` re-run verification
  without publishing.

The published archive install was smoke-verified end to end: checksum match
against the uploaded `SHA256SUMS`, extraction, and running the binary.

## Release command matrix

| Surface | Status | Evidence |
|---|---|---|
| `witchy --version` | PASS | Installed-archive smoke reports `witchy 0.1.0` on the exact release commit. |
| `witchy check` | PASS | Installed-archive smoke accepts valid source and rejects invalid syntax. |
| `witchy fmt` / `fmt --check` | PASS | Smoke rejects non-canonical input, formats it, then accepts it. |
| Direct source execution | PASS | Extracted binary prints the pinned `release-smoke` output. |
| `witchy test` | PASS | Extracted binary runs a temporary project's in-language test. |
| Project `build` / `run` | PASS | Embedded package front end builds and runs a temporary local project using no checkout inputs. |
| Portable WASM compile / sandbox | PASS | Extracted binary emits `.wasm`; the same binary runs it with expected output. |
| [`trusted-exe`](rfcs/0092-trusted-application-executables.md) | PASS | Exercised per target by `release-smoke.sh` in every native artifact job. |
| Backend parity | PASS | The release workflow's parity sweep differentially checks every runnable example on the tagged commit; the full gate was green before any artifact was built. |
| Remote Coven lifecycle | EXCLUDED | Implemented and e2e-tested against loopback, but not part of the 0.1 installability promise. Hosting it is post-0.1.0 work ([RFC-0116](rfcs/0116-hosted-coven-registry-m1.md), in flight). |
| Grimoire/Coven integrated install ([RFC-0095](rfcs/0095-grimoire-trusted-application-installation.md)) | EXCLUDED | Proposed behavior, not independently implemented and proven. |

## Platform matrix

| Target | Native runner | Status | Evidence |
|---|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` x64 | PASS | Native build + installed-archive smoke in the release workflow; archive published with checksum. |
| `aarch64-apple-darwin` | `macos-15` arm64 | PASS | Native build + installed-archive smoke in the release workflow; archive published with checksum. |
| `x86_64-apple-darwin` | `macos-15-intel` x64 | PASS | Native build + installed-archive smoke in the release workflow; archive published with checksum. |
| Windows | none | EXCLUDED | Not an implemented and natively verified 0.1 target. |

## Post-0.1.0 status

- **Publication authority:** unchanged for future releases — a tag push only
  produces a draft; a human-approved `workflow_dispatch` `action=publish` is
  required to go live. Tag reruns cannot replace a published release's assets.
- **Hosted registry:** the self-hosted coven registry and pm client are the
  next infrastructure step; RFC-0116 milestone 1 (scheme-aware HTTPS client
  addressing, live JWKS-over-HTTPS issuer discovery, Fly.io deploy artifacts)
  is in flight. See [rfcs/0116-hosted-coven-registry-m1.md](rfcs/0116-hosted-coven-registry-m1.md).
- **Filesystem TOCTOU:** RFC-0103's descriptor/handle-anchored authority is in
  the shipped release; deterministic parent and leaf replacement regressions
  cover both backends.
- The gitignored bug ledger remains split and internally stale. Release
  disposition comes from deterministic reproducers and this matrix, not raw
  OPEN/FIXED counts.
