# Stage releases from a pinned GitHub workflow

Copy [stage.yml](stage.yml) to `.github/workflows/stage.yml` in the trusted
automation repository. Review the two GitHub-maintained actions at their
pinned commits. Use a current Actions runner capable of running those
actions. The reusable job uses the `release` environment and the
`coven-release` self-hosted runner group. Restrict the group to the enrolled
repositories and the exact reviewed workflow commit. Use a dedicated clean
runner with access to the staging origin. Restrict who can push release
tags and authorize deployment to the `release` environment. [GitHub runner groups](https://docs.github.com/en/enterprise-cloud@latest/actions/how-tos/manage-runners/self-hosted-runners/manage-access)

In the package repository, save the caller as
`.github/workflows/release.yml`. Replace the example repository and all-zero
SHA with the automation repository and its full 40-digit commit. Set
`caller_workflow`, `job_workflow_ref` and `job_workflow_sha` in the
[independent enrollment](../../../witchy2/ci-staging.md#enroll-a-package)
to the resulting paths and commit.

```yaml
name: Stage release
on:
  push:
    tags: ['v*']
permissions:
  contents: read
  id-token: write
jobs:
  stage:
    uses: owner/automation/.github/workflows/stage.yml@0000000000000000000000000000000000000000
```

The job checks out the event commit without persisting checkout
credentials. It packs the selected library and passes the identity
returned by GitHub's toolkit directly to Coven on standard input. Running
the job does not execute package code or require compiler access. Coven
uses its pinned Witchy signer and HTTPS transport to stage the exact
bundle. It logs the staged digest for independent review. [GitHub OIDC reference](https://docs.github.com/en/actions/reference/security/oidc)

## Prepare the runner

On the trusted build machine, build the runner archive for the runner's
Linux architecture.

```sh
WITCHYC=/path/to/witchyc deploy/witchy2/build-runtime.sh runner \
  /path/to/linux/witchy-wasmtime-host ./runner-release
```

Use the [independent archive digest procedure](../README.md#build-and-install-the-runtime)
to verify and extract `coven-runner.tar`. Install the extracted
`opt/coven` as the root-owned runtime tree on the runner. This role has no
systemd services. Keep the runtime and configuration writable only by
operators and the signing seed readable only by the dedicated runner user.

- Install the enrolled 32-byte seed at
  `/var/lib/coven-runner/runner.seed`, owned by the runner user with mode
  `0400`.
- Install the independent staging CA bundle at `/etc/coven/runner-ca.pem`.
  Provision its approved SHA-384 digest separately as `ca_pin` in the runner configuration.
- Install `/etc/coven/runner.json` as a root-owned file readable by the
  runner. Copy the two independent module pins from the verified archive's
  `pins/runner.env`. Set `origin` and `registry` to the enrolled values.
  Set `package_dir` to a directory inside the checkout, or `.` for its root.

```json
{
  "origin": "https://staging.example.com",
  "registry": "my-registry",
  "package_dir": ".",
  "ca_pin": "sha384:<approved CA bundle digest>",
  "signer_pin": "sha384:<COVEN_CI_SIGNER_PIN digest>",
  "transport_pin": "sha384:<COVEN_CI_TRANSPORT_PIN digest>"
}
```

The checked-out package source, GitHub token, and enrolled runner key are
enough to stage a candidate. Publisher, witness, maintainer, and TUF keys
stay outside the runner. Provision the GitHub JWKS snapshot independently
on the staging service. The workflow accepts GitHub's normal job context
and gives the staging transport no GitHub endpoint or runner token.
