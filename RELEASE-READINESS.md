# Witchy 0.1.0 release readiness

This is the executable-evidence ledger for the private 0.1.0 release. RFC status
is not acceptance evidence. `PASS` requires a command or workflow result on the
recorded candidate commit; `FAIL` blocks publication; `EXCLUDED` is deliberately
outside the release contract.

Candidate commit: to be recorded after the release-engineering branch merges.

## Candidate command matrix

| Surface | Status | Required evidence |
|---|---|---|
| `witchy --version` | PASS | Canonical extracted-archive smoke on `aarch64-apple-darwin` reports `witchy 0.1.0` plus exact commit `4d97a8b4bf62cbbeb791387b739b99d4dc9b0aac`. Must repeat on final commit. |
| `witchy check` | PASS | Installed-archive smoke accepts valid source and rejects invalid syntax. |
| `witchy fmt` / `fmt --check` | PASS | Smoke rejects non-canonical input, formats it, then accepts it. |
| Direct source execution | PASS | Extracted binary prints the pinned `release-smoke` output. |
| `witchy test` | PASS | Extracted binary runs a temporary project's in-language test. |
| Project `build` / `run` | PASS | Embedded package front end builds and runs a temporary local project using no checkout inputs. |
| Portable WASM compile / sandbox | PASS | Extracted binary emits `.wasm`; the same binary runs it with expected output. |
| `trusted-exe` | PASS | Source-deleted, moved artifact works with empty `PATH`; argv, binding failures, `Dir` escapes, and corruption are exercised. |
| Backend parity | FAIL | Exact final release commit has not yet passed the serialized release gate and tag workflow parity job. |
| Remote Coven lifecycle | EXCLUDED | Existing implementation remains available but is not part of the 0.1 installability promise. |
| Grimoire/Coven integrated install | EXCLUDED | Proposed RFC-0095 behavior is not independently implemented and proven. |
| Proposed existential / `Dynamic` / lexical extensions | EXCLUDED | No proposed semantic is advertised by this release. |

## Platform matrix

| Target | Native runner | Status | Required evidence |
|---|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` x64 | FAIL | No native installed-archive smoke result exists yet. |
| `aarch64-apple-darwin` | `macos-15` arm64 | PASS | Local native archive smoke passed on commit `4d97a8b4bf62cbbeb791387b739b99d4dc9b0aac`; tag workflow must repeat on final commit. |
| `x86_64-apple-darwin` | `macos-15-intel` x64 | FAIL | Runner availability and native installed-archive smoke remain unproven. |
| Windows | none | EXCLUDED | Not an implemented and natively verified 0.1 target. |

## Known blockers and accepted limitations

- **BLOCKER — exact release commit:** the release-engineering slice must merge,
  then the serialized release gate must pass on fresh, clean `master`.
- **BLOCKER — native artifacts:** all three native workflow jobs and canonical
  archive smoke tests must pass. Intel macOS runner availability is unproven
  until the private repository accepts `macos-15-intel`. Run the private
  workflow manually with `action=verify-candidate` and the exact merged commit;
  that path has no publishing job and creates no tag or release.
- **BLOCKER — private GitHub authentication:** the current local `gh` credentials
  are invalid. Authenticated repository/release access must be restored before
  tag approval or uploaded-artifact verification.
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

- release commit SHA and annotated tag object;
- serialized gate journal event and log;
- workflow run URL and each native job result;
- exact four release assets and `SHA256SUMS` contents;
- authenticated post-upload download digest and clean-directory smoke result;
- private release URL plus final known limitations.
