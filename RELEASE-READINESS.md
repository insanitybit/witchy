# Witchy 0.1.0 release readiness

This is the executable-evidence ledger for the private 0.1.0 release. RFC status
is not acceptance evidence. `PASS` requires a command or workflow result on the
recorded candidate commit; `FAIL` blocks publication; `EXCLUDED` is deliberately
outside the release contract.

Candidate commit: `d7bee035f521429868240cdf4d55fccae4814311`.

## Candidate command matrix

| Surface | Status | Required evidence |
|---|---|---|
| `witchy --version` | PASS | Canonical extracted-archive smoke on `aarch64-apple-darwin` reports `witchy 0.1.0` plus exact candidate commit `d7bee035f521429868240cdf4d55fccae4814311`. |
| `witchy check` | PASS | Installed-archive smoke accepts valid source and rejects invalid syntax. |
| `witchy fmt` / `fmt --check` | PASS | Smoke rejects non-canonical input, formats it, then accepts it. |
| Direct source execution | PASS | Extracted binary prints the pinned `release-smoke` output. |
| `witchy test` | PASS | Extracted binary runs a temporary project's in-language test. |
| Project `build` / `run` | PASS | Embedded package front end builds and runs a temporary local project using no checkout inputs. |
| Portable WASM compile / sandbox | PASS | Extracted binary emits `.wasm`; the same binary runs it with expected output. |
| [`trusted-exe`](rfcs/0092-trusted-application-executables.md) | PASS | Source-deleted, moved artifact works with empty `PATH`; argv, binding failures, `Dir` escapes, and corruption are exercised. |
| Backend parity | FAIL | The exact candidate passed the landing gate's browser parity check (123 runnable blocks, zero divergence), but its serialized full release gate is red and the private candidate workflow has not run. |
| Remote Coven lifecycle | EXCLUDED | Existing implementation remains available but is not part of the 0.1 installability promise. |
| Grimoire/Coven integrated install ([RFC-0095](rfcs/0095-grimoire-trusted-application-installation.md)) | EXCLUDED | Proposed behavior is not independently implemented and proven. |
| Proposed existential / `Dynamic` / lexical extensions | EXCLUDED | No proposed semantic is advertised by this release. |

## Platform matrix

| Target | Native runner | Status | Required evidence |
|---|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` x64 | FAIL | No native installed-archive smoke result exists yet. |
| `aarch64-apple-darwin` | `macos-15` arm64 | PASS | Local native archive smoke passed on exact candidate `d7bee035f521429868240cdf4d55fccae4814311`; the private candidate workflow must repeat it. |
| `x86_64-apple-darwin` | `macos-15-intel` x64 | FAIL | Runner availability and native installed-archive smoke remain unproven. |
| Windows | none | EXCLUDED | Not an implemented and natively verified 0.1 target. |

## Known blockers and accepted limitations

- **BLOCKER — full acceptance gate:** the release-engineering slice merged and
  its ordinary serialized landing gate passed. The coordinator-locked
  `./scripts/check.sh --full` run on the exact candidate passed 2,373 workspace
  tests, formatting, clippy, browser parity, four sanitizer/fuzz stages, the
  from-scratch release build, and its Cargo tests. It then failed when
  `scripts/e2e-full.sh` generated `print(console, ...)`; current compiler
  behavior requires the method form `console.print(...)`. This acceptance
  fixture mismatch is owned outside the release-engineering slice and blocks
  publication until fixed and the exact-master full gate is rerun.
- **BLOCKER — merge queue:** [`impl/rfc0081-wasm-witness-adapters`](rfcs/0081-existential-trait-values.md) remains queued
  but blocked by the red compiler-owned `impl/rfc0081-wasm-witness-dispatch`
  attempt. Proposed existential work is excluded from 0.1 and has not landed,
  but the final release procedure requires an empty queue.
- **BLOCKER — native artifacts:** all three native workflow jobs and canonical
  archive smoke tests must pass. Intel macOS runner availability is unproven
  until the private repository accepts `macos-15-intel`. Run the private
  workflow manually with `action=verify-candidate` and the exact merged commit;
  that path has no publishing job and creates no tag or release.
- **BLOCKER — private GitHub authentication:** the current local `gh` credentials
  are invalid. Authenticated repository/release access must be restored before
  tag approval or uploaded-artifact verification.
- **BLOCKER — remote candidate reachability:** authenticated SSH confirms the
  private `origin/master` is `0808a0e6013c448d4837ed93909f68ac4f5f9d49`,
  1,092 commits behind the candidate. The pre-tag private candidate workflow
  cannot run until the default branch is intentionally synchronized. Pushing
  that history is not implicit release-engineering authority.
- **BLOCKER — publication authority:** no tag or release may be created until the
  user approves the exact green commit. The tag workflow creates a draft; final
  publication is a separate explicit workflow dispatch.
- **ACCEPTED LOW limitation — filesystem race:** `Dir` confinement is not
  TOCTOU-free under concurrent local symlink replacement. It remains documented
  in the changelog rather than being expanded into compiler/runtime work here.
- The gitignored bug ledger is split and known to be internally stale. Release
  disposition therefore comes from deterministic reproducers and the candidate
  matrix, not raw OPEN/FIXED counts.

## Final evidence to record

Local exact-candidate ARM evidence currently records archive SHA-256
`b246b8fe7398c3048adacfb34a0c9fc8a1b907d4f3001aecd0e03804ea5ca808`.
This is not an uploaded release checksum and must not be presented as one.

- release commit SHA and annotated tag object;
- serialized gate journal event and log;
- workflow run URL and each native job result;
- exact four release assets and `SHA256SUMS` contents;
- authenticated post-upload download digest and clean-directory smoke result;
- private release URL plus final known limitations.
