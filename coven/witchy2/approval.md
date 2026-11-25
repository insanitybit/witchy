# Private candidate staging and release approval

Coven stores a packed candidate by the SHA-384 of its canonical bytes.
A separate maintainer ceremony uses WebAuthn to approve those exact bytes.
The staged candidate and assertion evidence live in a private SQLite store.
The loopback registry API and the Glamour catalog continue to use their
existing signed-snapshot protocol.

## Maintainer browser review

Run the review service on the dedicated HTTPS origin recorded in the release
policy. Use a discoverable passkey provisioned for that origin and package.
The first user-verified assertion opens the candidate. The page shows the
digest, CI provenance when present, and the source changes from the most
recent lower version in the active TUF mirror. The second assertion uses
the same credential and commits to the displayed registry, coordinate, and
digest. Both challenges are single-use. The page has no session cookie.

Mount the publisher's verified mirror as a read-only tree, separate from
private staging. Provision the initial TUF root, release trust file, and
their independent pins. The service verifies the signed mirror metadata
and the preceding bundle before displaying its diff. It keeps an owner-only
checkpoint in `.review-baseline/history` under private staging. Once the
service sees a signed generation, the Witchy worker rejects missing
timestamp metadata, older versions, and changes to the bytes of an already
accepted root or metadata version. Keep the checkpoint with the staging
store across service restarts and moves. On first use the service trusts
the first valid, unexpired generation it sees.

```sh
export COVEN_RELEASE_STAGING=/var/lib/coven/staging
export COVEN_RELEASE_MIRROR=/var/lib/coven/published-read-only
export COVEN_RELEASE_TRUST=/etc/coven/release-trust.json
export COVEN_RELEASE_TRUST_PIN=$trust_pin
export COVEN_RELEASE_ROOT=/etc/coven/initial-root.json
export COVEN_RELEASE_ROOT_PIN=$root_pin

src="$prefix/libexec/coven"
for entry in review_https review_read_challenge review_inspect review_commit tuf_review_baseline; do
  witchyc wasm-caps --policy "$src/$entry.policy" "$src/$entry.wy"
done
witchyc build "$src/review_https.wy" -o review.wasm \
  --heap-pages=352 --slots=512 --stack-bytes=65536
witchyc build "$src/review_read_challenge.wy" -o review-challenge.wasm \
  --heap-pages=128 --slots=65536 --stack-bytes=65536
witchyc build "$src/review_inspect.wy" -o review-inspect.wasm \
  --heap-pages=64 --slots=4096 --stack-bytes=65536
witchyc build "$src/review_commit.wy" -o review-commit.wasm \
  --heap-pages=64 --slots=4096 --stack-bytes=65536
witchyc build "$src/tuf_review_baseline.wy" -o review-baseline.wasm \
  --heap-pages=448 --slots=65536 --stack-bytes=65536
review_pin="sha384:$(shasum -a 384 review.wasm | cut -d' ' -f1)"
challenge_pin="sha384:$(shasum -a 384 review-challenge.wasm | cut -d' ' -f1)"
inspect_pin="sha384:$(shasum -a 384 review-inspect.wasm | cut -d' ' -f1)"
commit_pin="sha384:$(shasum -a 384 review-commit.wasm | cut -d' ' -f1)"
baseline_pin="sha384:$(shasum -a 384 review-baseline.wasm | cut -d' ' -f1)"
coven release review-serve --listen 127.0.0.1:8444 \
  --staging-dir "$COVEN_RELEASE_STAGING" --mirror "$COVEN_RELEASE_MIRROR" \
  --trust "$COVEN_RELEASE_TRUST" --trust-pin "$COVEN_RELEASE_TRUST_PIN" \
  --root "$COVEN_RELEASE_ROOT" --root-pin "$COVEN_RELEASE_ROOT_PIN" \
  --chain "$chain" --tls-key "$tls_key" --module review.wasm --module-pin "$review_pin" \
  --challenge-module review-challenge.wasm --challenge-pin "$challenge_pin" \
  --inspect-module review-inspect.wasm --inspect-pin "$inspect_pin" \
  --commit-module review-commit.wasm --commit-pin "$commit_pin" \
  --baseline-module review-baseline.wasm --baseline-pin "$baseline_pin"
# In a separate shell with COVEN_RELEASE_STAGING set:
coven release review-url --staging-dir "$COVEN_RELEASE_STAGING" "$digest"
```

Open the returned URL at the configured origin. The review commit guest
reads and validates the installed policy with a policy-only SQLite grant
when the listener starts and when a browser requests a challenge. The
independently pinned challenge guest binds that challenge to the installed
policy under the SQLite write lock. Both browser challenge operations
lack access to candidate and approval records. The local CLI uses a
separate Witchy issuer for approval and enrollment challenges. For
`review-url` and local approval challenge issuance, a read-only Witchy
transaction checks the candidate bytes, coordinates, and digest against
the same snapshot as the active policy and derives the scoped credential
list. The independently pinned
inspection guest verifies the passkey before consuming the read challenge,
updating the counter, and issuing an approval challenge scoped to the
reviewer in a single SQLite transaction. The independently pinned
review commit guest selects the policy, challenge, staged candidate, and
authenticator counter in one read-only transaction for inspection and
approval. For inspection it also selects available CI provenance and
checks that it names the same release and registry. The browser receives
candidate bytes after the inspection assertion commits. A review-only
legacy store can serve without CI provenance. Restart the review service
after adding CI tables through `coven release init`.
The review commit guest's write mode verifies
the WebAuthn assertion using that snapshot. Under SQLite's write lock it
rechecks the policy, challenge and
expiry, staged coordinate, reviewing credential, and counter before
recording the receipt and consuming the challenge. The commit worker returns
the saved receipt from that transaction. `coven release receipt` uses a
read-only Witchy worker to check the candidate bytes and their coordinates
against the archived receipt. The receipt is also available for TUF sealing.
Restart the review service when changing its baseline trust file. When
upgrading the review service, deploy the matching Wasm host and rebuild the
pinned `review-commit.wasm` together with the launcher.
Use the length-prefixed DER chain and owner-private P-384 PKCS#8 key format
described in [CI staging](ci-staging.md). The Witchy listener serves public
assets, renders the review and enrollment pages, verifies HTTP origin and framing, and sends a single action request
over an inherited Unix stream. It can sign TLS handshakes without reading
the key, staging store, or published mirror. The standard Witchy HTTPS
writer frames both HTML and JSON responses. The page renderer uses fixed
markup and derives script and stylesheet integrity hashes from the installed
assets. The CSP admits one script hash and the exact stylesheet URL per
page. Review can connect to its three action URLs. Enrollment works with
local files and has no network connections. Both policies forbid Trusted
Types policy creation. The browser scripts and stylesheet ship with Coven
and are trusted installation assets. The action broker passes public database snapshots to the Witchy workers
and presents the source comparison. An independently pinned Witchy worker checks
the assertion using the public key enrolled for that package. An
independently pinned baseline worker verifies the signed metadata and sealed
comparison bundle.
The assertion worker receives
only the assertion, challenge envelope, expected origin, public key, and
stored counter. Provision this origin's TLS key and registration policy
independently from the CI origin.

## Local operator workflow

Use an owner-only staging directory on a trusted local filesystem.
Use the installed `coven` command with a matching Witchy Wasm host.
The examples below build workers locally. For a compiler-free operator
machine, provision the [pinned workers](#operate-without-a-compiler).
The [hashed Python lock](approval-requirements.txt) supplies the test fixtures
and interoperability checks. The following commands assume an authenticated
local operator and out-of-band enrollment of each package maintainer.

```sh
coven pack path/to/package > candidate.coven
coven stage --staging-dir ./staging candidate.coven
coven release set-policy --staging-dir ./staging verified-policy.json
coven release challenge --staging-dir ./staging "$digest" > challenge.json
coven release approve --staging-dir ./staging -- "$challenge" assertion.json
coven release receipt --staging-dir ./staging "$digest"
```

The local staging worker validates the packed files, hashes the submitted bytes,
and stores the candidate in a single Witchy invocation. It shares package
validation and the candidate write with CI staging. The worker can read and
insert only candidate records. An identical submission returns the same digest.

Use the digest printed by `stage` and the `publicKey.challenge` printed by
`challenge`. For a browser test, call `navigator.credentials.get` with the
challenge's public-key options and serialize the resulting credential as JSON.
The challenge response also contains the canonical intent and expected
origin so a trusted approval page can show what is being signed.

The operator's policy JSON specifies `registry`, a monotonically increasing
`revision`, the exact release `origin`, and a `credentials` object keyed by
canonical unpadded base64url credential ID. Each entry contains `public_key`
(the base64url COSE EC2 public key obtained by verified ES256 enrollment) and
`packages` (an explicit list of names). Use a dedicated HTTPS release origin.
The verifier uses its exact hostname as RP ID. The local development
allowance is `http://localhost` with an optional port. Registry identity and
origin stay fixed for the private store.

The local `coven release set-policy` command passes the UTF-8 policy file to a
Witchy worker. The worker reads and validates the active policy, then compares
its exact bytes under the SQLite write lock before installing a new revision.
The registry and origin stay fixed across revisions. Changes must advance
the revision and preserve the public key of each recorded credential ID.
Reinstalling an identical canonical policy preserves outstanding challenges.
A changed policy resets those challenges atomically.

## Enroll or replace a maintainer passkey

Initialize a new registry policy with its registry ID, HTTPS review origin,
revision `1`, and an empty `credentials` object. Start the review listener
on that origin and issue one enrollment request per maintainer. The operator
selects the package scope and exchanges the request and response with the
identified maintainer over an authenticated channel.

```sh
coven release enroll-start --staging-dir "$COVEN_RELEASE_STAGING" \
  --package demo > enrollment-request.json
coven release enroll-finish --staging-dir "$COVEN_RELEASE_STAGING" \
  enrollment-response.json > enrolled.json
```

The maintainer opens the request's `/enroll` URL, selects the request file,
creates an ES256 discoverable passkey, and confirms it with a second
WebAuthn operation. They save the response from the page and return it to
the operator. The page works with local files. The local operator CLI runs
a read-only Witchy worker to select the active policy for issuance and
validate the stored challenge for completion. The write invocation checks
possession, derives the new policy, and rechecks the exact policy and
challenge under the SQLite write lock. It installs the key and counter,
advances the policy revision, and invalidates the old challenges in one
transaction. The request expires after five minutes and binds the registry,
origin, package scope, and current policy.
[WebAuthn credential creation and assertions](https://www.w3.org/TR/webauthn-3/#sctn-api)

For recovery, add `--replace OLD_CREDENTIAL_ID` to `enroll-start` and
specify the old credential's packages. The operator authenticates the
maintainer before issuing the request. Finishing the ceremony installs the
new key and removes the old key from active policy together. The old
credential's stored counter persists. The ceremony proves possession and
user verification for the new passkey. Hardware attestation remains an
independent enrollment policy decision.

`enroll-finish` returns the new policy as `policy` and its canonical digest.
Use [TUF policy rotation](tuf.md#rotate-the-maintainer-policy) to provision
the updated release trust and preserve selected historical bundles.

### Operate without a compiler

Build and check the workers on a trusted build machine. Install the Wasm
files on the operator machine and provision their SHA-384 pins independently.
Choose the primary worker by command and pass `--module FILE --module-pin PIN`.

| `coven release` command | Primary worker | Additional challenge worker |
| --- | --- | --- |
| `set-policy` | `review_set_policy.wasm` | |
| `enroll-start` | `review_enroll.wasm` | `review_issue_challenge.wasm` |
| `enroll-finish` | `review_enroll.wasm` | |
| `review-url`, `approve` | `review_commit.wasm` | |
| `challenge` | `review_commit.wasm` | `review_issue_challenge.wasm` |
| `receipt` | `review_receipt.wasm` | |

For a locally packed candidate, `coven stage` also accepts
`--module FILE --module-pin PIN` with `stage_candidate.wasm`. The same
operator machine can use [pinned publisher and witness workers](tuf.md#sign-and-seal-without-a-compiler).

For `enroll-start` and `challenge`, pass the additional worker as
`--challenge-module FILE --challenge-pin PIN`. Coven requires both worker
pairs when either is supplied. This is the local issuer, separate from the
pinned browser issuer that cannot read candidate records.

```sh
src="$prefix/libexec/coven"
for worker in review_set_policy review_enroll review_commit review_issue_challenge review_receipt; do
  witchyc wasm-caps --policy "$src/$worker.policy" "$src/$worker.wy"
  case "$worker" in
    review_set_policy|review_issue_challenge) heap=128 slots=65536 ;;
    *) heap=64 slots=4096 ;;
  esac
  witchyc build "$src/$worker.wy" -o "$worker.wasm" \
    --heap-pages="$heap" --slots="$slots" --stack-bytes=65536
done
witchyc wasm-caps --policy "$src/stage_candidate.policy" "$src/stage_candidate.wy"
witchyc build "$src/stage_candidate.wy" -o stage_candidate.wasm \
  --heap-pages=96 --slots=4096 --stack-bytes=65536
```

On the operator machine, provision the store separately using
[pinned `coven release init`](ci-staging.md#run-the-staging-service).
Pinned staging and policy installation use the existing store. Schema
provisioning has its own worker and grant.

```sh
coven release set-policy --staging-dir "$COVEN_RELEASE_STAGING" \
  --module "$policy_module" --module-pin "$policy_pin" verified-policy.json
coven release enroll-start --staging-dir "$COVEN_RELEASE_STAGING" \
  --module "$enroll_module" --module-pin "$enroll_pin" \
  --challenge-module "$issuer_module" --challenge-pin "$issuer_pin" \
  --package demo > enrollment-request.json
coven release enroll-finish --staging-dir "$COVEN_RELEASE_STAGING" \
  --module "$enroll_module" --module-pin "$enroll_pin" enrollment-response.json
```

## Approval evidence

The challenge is SHA-384 over a domain separator, a random nonce, and the
canonical release intent. It commits to the release operation, registry,
package, exact version, staged payload digest, independently installed
policy digest, and expiry. Origin, RP ID, signature, user verification,
scope and the authenticator counter are checked at approval. A change to
policy invalidates outstanding challenges. The verifier accepts two zero
counters for credentials without a counter. Once either counter is positive,
the signed counter must increase. The challenge is consumed in
the same transaction that updates the counter and writes the receipt.
A competing candidate for the same version cannot replace the approval.
[WebAuthn signature counters](https://www.w3.org/TR/webauthn-3/#sctn-sign-counter)

The receipt retains the signed assertion and canonical intent. A witness
independently binds that evidence to the release and signs when it was observed.
The TUF signer and Grimoire independently verify the witness and the underlying
assertion. An assertion records user verification. Enrollment policy owns any
additional requirements for hardware, backups, or attestation. [TUF release evidence](tuf.md)

## Production work still open

Operators authenticate maintainers before issuing enrollment requests.
Operators distribute the new release trust file and pin. They provision runners and their keys
through the dedicated HTTPS staging service. A separate TUF mirror serves
releases after the maintainer ceremony and signing.
[CI staging](ci-staging.md) · [TUF publication](tuf.md)
