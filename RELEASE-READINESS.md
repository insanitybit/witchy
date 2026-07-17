# Witchy 0.1.0 release readiness

This is the executable-evidence ledger for the private 0.1.0 release. RFC status
is not acceptance evidence. `PASS` requires a command or workflow result on the
recorded candidate commit; `FAIL` blocks publication; `EXCLUDED` is deliberately
outside the release contract.

Last packaged evidence commit: `d7bee035f521429868240cdf4d55fccae4814311`.
This is not yet the final release candidate; the final candidate is the exact
queue-settled master commit that passes every gate below.

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
| `x86_64-apple-darwin` | `macos-15-intel` x64 | FAIL | [GitHub documents `macos-15-intel` as a standard Intel runner for private repositories](https://docs.github.com/en/actions/reference/runners/github-hosted-runners), but repository scheduling and native installed-archive smoke remain unproven until the candidate workflow runs. |
| Windows | none | EXCLUDED | Not an implemented and natively verified 0.1 target. |

## Known blockers and accepted limitations

- **BLOCKER — exact-master full acceptance gate:** the release-engineering slice
  and acceptance-fixture refresh have merged through the coordinator. The
  earlier coordinator-locked `./scripts/check.sh --full` run passed 2,373
  workspace tests, formatting, clippy, browser parity, four sanitizer/fuzz
  stages, the from-scratch release build, and its Cargo tests before exposing
  the stale fixture. Commit `eb561018e254d9940bcef45d89a80c40a3a58f2a`
  corrected the fixture without compiler changes, passed all 31 direct
  `./scripts/e2e-full.sh --quick` checks, and subsequently passed its serialized
  landing gate. Publication remains blocked until `./scripts/check.sh --full`
  passes on the exact final master commit.
- **BLOCKER — merge queue:** the repository still has a large active queue and
  the coordinator is gating it serially. Proposed semantic work, including
  [RFC-0081 existential trait values](rfcs/0081-existential-trait-values.md), is
  excluded from this release-engineering scope, but the final release procedure
  requires an empty queue and no pending release blocker.
- **BLOCKER — native artifacts:** all three native workflow jobs and canonical
  archive smoke tests must pass. GitHub documents `macos-15-intel` as a
  standard Intel runner label for private repositories, but this repository has
  not yet scheduled the job or executed the native smoke. Run the private
  workflow manually with `action=verify-candidate` and the exact merged commit;
  that path has no publishing job and creates no tag or release.
- **BLOCKER — private GitHub authentication:** the current local `gh` credentials
  are invalid. Authenticated repository/release access must be restored before
  tag approval or uploaded-artifact verification.
- **BLOCKER — remote candidate reachability:** authenticated SSH confirms the
  private `origin/master` is `0808a0e6013c448d4837ed93909f68ac4f5f9d49`,
  behind the current local master. The pre-tag private candidate workflow cannot
  run until the default branch is intentionally synchronized. Pushing that
  history is authorized only after the exact queue-settled master is green; no
  tag push is authorized.
- **BLOCKER — Pages visibility before remote sync:** the ordinary
  [CI workflow](.github/workflows/ci.yml) already on `origin/master` deploys the
  docs bundle with `pages: write` after a master push. The canonical public
  Pages endpoint returned HTTP 404 on 2026-07-18, so no public exposure was
  observed. The live GitHub repository record identifies Witchy as a private
  personal-account repository, while [GitHub documents private Pages access
  control only for organization-owned private or internal project
  sites](https://docs.github.com/en/enterprise-cloud@latest/pages/getting-started-with-github-pages/changing-the-visibility-of-your-github-pages-site).
  Disable the Pages deployment through its owning lane before synchronizing
  master; fail closed rather than risk creating a public documentation host.
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
