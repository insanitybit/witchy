# Private Coven staging from GitHub Actions

The `github-actions-v1` profile stages a packed Witchy library on a dedicated
HTTPS origin. It requires a GitHub-issued identity token and an ML-DSA-87
signature from a separately enrolled runner key. The first authorized upload
consumes both the issuer's token ID and a short-lived Coven challenge in the
same SQLite transaction as the candidate and its receipt. CI cannot approve a
release through this service.

## Enroll a package

Use a self-hosted runner assigned only to the trusted release workflow. Generate
its 32-byte seed independently of the GitHub token. Store the seed as an
owner-only file on the runner. Use `coven ci public-key --key runner.seed` to
export the corresponding public key for independent enrollment. Keep this key
separate from publisher, maintainer, and TUF signing credentials.

The operator provisions a configuration and its SHA-384 pin through a trusted
channel. The pinned configuration has this structure:

```json
{
  "profile": "github-actions-v1",
  "registry": "my-registry",
  "origin": "https://staging.example.com",
  "jwks_pin": "sha384:<96 lowercase hex digits>",
  "packages": {
    "my-package": {
      "sub": "repo:owner/repo:environment:release",
      "repository_id": "123456",
      "repository_owner_id": "98765",
      "environment": "release",
      "caller_workflow": "owner/repo/.github/workflows/release.yml",
      "job_workflow_ref": "owner/automation/.github/workflows/stage.yml@<commit>",
      "job_workflow_sha": "<same 40 lowercase hex commit digits>",
      "key": "<public key exported by coven ci public-key>"
    }
  }
}
```

Pin the exact subject configured for the repository, including its actual
GitHub subject format. The OIDC audience for this example is
`https://staging.example.com/v1/ci/my-registry`. The job must be a tag push in
its enrolled environment. The tag must be `v` followed by the exact package
version. The caller path is pinned and the reusable workflow is pinned by
commit. GitHub's `self-hosted` claim identifies the runner class. The
independently enrolled key proves possession and supplies no hardware
attestation. [GitHub Actions OIDC claims](https://docs.github.com/en/actions/reference/security/oidc)

Provision and pin a JWKS snapshot from GitHub's reported HTTPS endpoint. The
Witchy verifier accepts RS256 from those keys and refuses unknown key IDs.
It verifies the original compact token bytes and checks issuer, audience,
expiry, token lifetime, and the enrolled workload. Rotate the JWKS pin and
restart the service through the same provisioning channel. Token headers
cannot supply issuer keys or direct the service to a URL.
The CI workers use the separately packaged [Witchy OIDC verifier](../../witchyc/packages/oidc/README.md) with an allocator grant. [GitHub OIDC discovery](https://token.actions.githubusercontent.com/.well-known/openid-configuration)

## Run the staging service

`coven ci serve` serves challenge, upload, and receipt routes over Witchy
TLS 1.3. Each connection gets fresh guest memory, a TLS signer, and one
exchange with the private staging process, which also issues challenges. The HTTPS
guest has no outbound network access or writable directory. Its local
transaction capability uses an inherited Unix stream with no listening
address. The staging process gives the original identity, proof, and
candidate bytes to an independently pinned Witchy transaction worker. That
worker authenticates the request and commits those same bytes within one
invocation. Validated SQLite bindings stay inside the worker.

Build the three Wasm modules from the installed sources and provision their
SHA-384 pins with the pinned configuration. [Coven dependencies](approval-requirements.txt)

```sh
src="$prefix/libexec/coven"
for entry in ci_https ci_identity ci_commit; do
  witchyc wasm-caps --policy "$src/$entry.policy" "$src/$entry.wy"
done
witchyc build "$src/ci_https.wy" -o ci-https.wasm \
  --heap-pages=32 --slots=256 --stack-bytes=65536
witchyc build "$src/ci_identity.wy" -o ci-identity.wasm \
  --heap-pages=128 --slots=4096 --stack-bytes=65536
witchyc build "$src/ci_commit.wy" -o ci-commit.wasm \
  --heap-pages=128 --slots=4096 --stack-bytes=65536
storage_pin="sha384:$(shasum -a 384 ci-commit.wasm | cut -d' ' -f1)"
listener_pin="sha384:$(shasum -a 384 ci-https.wasm | cut -d' ' -f1)"
identity_pin="sha384:$(shasum -a 384 ci-identity.wasm | cut -d' ' -f1)"
```

Provision the private database as the service account before bringing up
the listener. The schema worker holds a separate table-creation grant.
On an existing store the same command adds missing tables and retains
existing contents. A host without a compiler can provision from the pinned
schema module built below.

```sh
witchyc wasm-caps --policy "$src/staging_schema.policy" "$src/staging_schema.wy"
witchyc build "$src/staging_schema.wy" -o staging-schema.wasm \
  --heap-pages=4 --slots=64 --stack-bytes=32768
schema_pin="sha384:$(shasum -a 384 staging-schema.wasm | cut -d' ' -f1)"
coven release init --staging-dir /var/lib/coven/staging \
  --module staging-schema.wasm --module-pin "$schema_pin"
```

The service account owns the private staging directory and the owner-only
32-byte nonce file. The CI configuration and JWKS must also be single-link
regular files owned by the service account. Install them with mode `0400`
and provision their pins independently. Issue the certificate for the
hostname in the pinned HTTPS origin. Provision the public leaf-first DER chain and an owner-only
P-384 PKCS#8 TLS key. The launcher opens both keys by descriptor. The HTTPS
host holds the TLS key, and the private Witchy challenge and transaction
workers hold the nonce key. The launcher derives the listener's HTTP
authority and the workers' challenge audience from the pinned configuration.
Restart the service when replacing a key or
certificate. [TLS identity format](tls13/README.md)

```sh
export COVEN_CI_CONFIG=/etc/coven/staging.json
export COVEN_CI_CONFIG_PIN=$config_pin
export COVEN_CI_JWKS=/etc/coven/github-jwks.json
export COVEN_CI_IDENTITY_MODULE=/etc/coven/ci-identity.wasm
export COVEN_CI_IDENTITY_PIN=$identity_pin
export WITCHY_WASMTIME_HOST="$prefix/bin/witchy-wasmtime-host"
export COVEN_CI_NONCE_KEY=/var/lib/coven/staging-nonce.key
export COVEN_CI_STAGING=/var/lib/coven/staging

coven ci serve --listen 127.0.0.1:8443 \
  --module /etc/coven/ci-https.wasm --module-pin "$listener_pin" \
  --storage-module /etc/coven/ci-commit.wasm --storage-pin "$storage_pin" \
  --chain /etc/coven/chain --tls-key /var/lib/coven/staging-tls.der
```

The example binds loopback. For external runners, make the staging origin
reachable only from authorized runner networks and apply connection and
request rate limits there. Supervise the launcher and restart it if the
private transaction stream closes. The host's listener closes when its
launcher exits. Keep CI staging, [maintainer review](approval.md), and the
[TUF mirror](tuf.md) on distinct origins and outside each other's served
roots. [HTTP/1.1 message framing](https://www.rfc-editor.org/rfc/rfc9112.html#section-6)

The broker checks the operator's independent configuration pin. The Witchy
identity module validates the configuration's profile, HTTPS origin, registry,
JWKS digest, issuer keys, and enrollments before issuing challenges. It has no
database authority. The transaction worker verifies the OIDC token,
nonce, enrolled signing key, and candidate before binding the SQL transaction.
Install the CI broker and transaction worker from the same release. Their
private request format starts with a three-byte big-endian length, followed by
exactly that many UTF-8 JSON bytes and then the candidate bytes. The worker
accepts at most 2 MiB of JSON, 1 MiB of candidate data, and 2,799,999 bytes
combined before the prefix. It requires EOF at the end of the declared body.
Its host grant opens the existing SQLite file and limits operations to the CI
tables and the candidate storage needed for uploads. The engine samples the commit
time after taking the write lock and rolls back if a statement fails.
The broker starts the listener after the storage worker checks the required
tables and columns with a read-only grant. The running service has no
schema-creation grant.
[SQLite authorizer](https://www.sqlite.org/c3ref/set_authorizer.html)

## Stage and recover a receipt

Use the [pinned reusable GitHub workflow and runner archive](../deploy/witchy2/github/README.md)
to stage from an enrolled tag job.

Give the reusable job `id-token: write`. Request an identity token for the
staging audience using GitHub's runner token endpoint. Pass the identity token
on standard input without logging it. `coven pack` builds the canonical release
record and source bytes that will be reviewed.

```sh
coven pack ./my-package > candidate.coven
coven ci stage candidate.coven --origin "$stage_origin" --key "$runner_seed" \
  --ca-pin "$ca_pin" < "$token_file"
```

The installed CLI runs the outbound TLS and HTTP exchange in Witchy. It
expects the Coven TLS 1.3 profile on a DNS hostname with a P-384 server
certificate. For a private CA, pass `--ca-file runner-ca.pem` to both
`stage` and `receipt`. With no override, the runner uses the PEM file
configured by `SSL_CERT_FILE` or the platform OpenSSL default PEM file.
Provision CA certificates in that file and send the leaf and any
intermediates from the staging server. The runner snapshots its CA file
for the challenge and authenticated request. Provision the approved PEM
digest independently and pass it as `--ca-pin` for either CA path. The
Witchy worker checks the exact staged bytes against that digest before
opening a connection. Without a configured pin, the client computes
one from its selected bundle and uses it for the entire exchange.
[PEM certificates](https://www.rfc-editor.org/rfc/rfc7468.html#section-5)

The listener uses Witchy’s checked HTTPS writer for fixed-length JSON,
including refusals. A method refusal carries `Allow: POST`. The client
accepts one bounded, fixed-length JSON response and waits for
TLS close_notify before using it. Redirects, conflicting length fields,
transfer encodings, and trailing application bytes are refused.
[HTTP/1.1 message framing](https://www.rfc-editor.org/rfc/rfc9112.html#section-6)

The CLI verifies HTTPS, obtains a fresh challenge, and signs the method, exact
URI, identity token digest, challenge, package, candidate digest, and body
digest. The runner's Witchy program hashes the body and token and builds the
same transcript as the Witchy service verifier. The Wasm host receives the seed
through an inherited descriptor and restricts signing to the CI request domain.
The runner needs an installed Witchy Wasm host. Its transport and signing
workers have separate deadlines and stop if the Coven launcher exits.
The default CLI builds the workers locally. Use [pinned runner workers](#run-ci-without-a-compiler)
on a machine without the compiler. The service validates the package before
the staging transaction. There is
no reusable release credential or access token issued by Coven. The receipt
records the candidate digest and authenticated run and workflow information.

If the upload result was lost, obtain a fresh GitHub identity token for the
same job and run:

```sh
coven ci receipt my-package "$candidate_digest" --origin "$stage_origin" \
  --key "$runner_seed" < "$fresh_token_file"
```

The lookup requires fresh key proof and returns the original receipt for that
job and digest. A successful upload still awaits independent maintainer approval.

### Run CI without a compiler

Build and check the workers on a trusted build machine. Provision these
Wasm files and their independent SHA-384 pins on the runner.

```sh
src="$prefix/libexec/coven"
witchyc wasm-caps --policy "$src/ci_sign.policy" "$src/ci_sign.wy"
witchyc wasm-caps --policy "$src/ci_post.policy" "$src/ci_post.wy"
witchyc build "$src/ci_sign.wy" -o ci_sign.wasm \
  --heap-pages=32 --slots=512 --stack-bytes=65536
witchyc build "$src/ci_post.wy" -o ci_post.wasm \
  --heap-pages=48 --slots=4096 --stack-bytes=65536
```

```sh
coven ci public-key --key "$runner_seed" \
  --signer-module "$signer_module" --signer-pin "$signer_pin"
coven ci stage candidate.coven --origin "$stage_origin" --key "$runner_seed" \
  --signer-module "$signer_module" --signer-pin "$signer_pin" \
  --transport-module "$transport_module" --transport-pin "$transport_pin" \
  --ca-pin "$ca_pin" < "$token_file"
```

Pass the same two worker pairs to `ci receipt`. When either pair is supplied
for `stage` or `receipt`, both are required. Coven checks them before
reading the identity token or opening the HTTPS connection. The HTTPS
worker receives the origin, CA file and its expected digest. It checks
the digest, checks the challenge nonce format,
and checks that successful stage and receipt replies match the requested
package and digest. The signer receives only the CI request signing grant
for the enrolled key. Wasmtime checks the exact module against its pin
on every public-key or signing request. When upgrading Coven, rebuild and
repin `ci_post.wasm` with the same release. The launcher requires the
verified CA digest in each successful worker reply.
