# Releasing witchy

[`.github/workflows/release.yml`](.github/workflows/release.yml) is the
pipeline; [RELEASE-READINESS.md](RELEASE-READINESS.md) records the design
rationale and the as-shipped 0.1.0 evidence. This page is the operator
procedure.

Releases are cut on the **public GitHub history** (`origin/master`), which is
disjoint from the local development `master`. Tag only commits that exist on
the pushed public history; never rebase local work onto `origin/master`.

## Cut a release

1. **Version bump, one commit.** On the public history, set the new `X.Y.Z` in
   every workspace `Cargo.toml` and include the regenerated `Cargo.lock` in
   the same commit. `./scripts/release-version.sh vX.Y.Z` must pass on that
   commit — the first two v0.1.1 tag pushes failed on exactly this (a missed
   member version, then a stale `Cargo.lock`).
2. **Push the commit, then tag it** with an **annotated** tag (the workflow
   rejects lightweight tags):

   ```sh
   git push origin <public-master-branch>
   git tag -a vX.Y.Z -m "Witchy X.Y.Z" <public-commit-sha>
   git push origin vX.Y.Z
   ```

3. **The tag push produces a draft, not a release.** It runs the full gate on
   the exact tagged commit (zizmor, build + clippy bug-lints + nextest,
   heap-check differential fuzz, backend parity sweep, from-scratch
   acceptance, `witchy fmt --check`, browser/docs bundle), builds each target
   on a **native** runner, smokes every installed archive, attests SLSA build
   provenance for each archive, and creates an immutable **draft** release
   with the target archives plus `SHA256SUMS`. Nothing is public yet.
4. **Publishing is a separate human act.** Run the Release workflow via
   `workflow_dispatch` with `action=publish`, the tag, and the exact
   40-character tagged commit SHA. It re-runs the entire gate and rebuilds
   every artifact on the same commit, byte-compares the rebuilt assets against
   the draft, and only then flips the draft public. `action=verify-draft` does
   the same without publishing; `action=verify-candidate` vets a commit before
   you tag it.
5. **Verify from the outside.** On a clean machine:

   ```sh
   curl --proto '=https' --tlsv1.2 -fsSL \
     https://raw.githubusercontent.com/insanitybit/witchy/master/install.sh | bash
   gh attestation verify --repo insanitybit/witchy witchy-X.Y.Z-<target>.tar.gz
   ```

## Invariants

- **Published assets are immutable.** Tag reruns byte-compare against the
  existing release and refuse to replace anything. A bad release means a new
  patch version, never a swapped asset.
- **The target set is exact.** The manifest job fails unless precisely these
  archives exist: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
  `aarch64-apple-darwin`, `x86_64-apple-darwin`.
- **No cross-compilation.** Every artifact job proves `rustc -vV`'s host
  matches the advertised target before building.

## Adding a target

Four places, all of which fail closed if you miss one:

1. A native-runner matrix row in `release.yml`'s `build` job.
2. The `expected` target list in the `manifest` job.
3. The asset list and both asset-count checks in the `publish` job.
4. The target allowlists in `scripts/release-package.sh` and
   `scripts/release-smoke.sh`, plus the platform detection in `install.sh`.
