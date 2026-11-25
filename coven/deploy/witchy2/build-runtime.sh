#!/bin/bash
# Build a Coven service archive on the trusted build machine.
set -euo pipefail
umask 022

if [[ $# != 3 || ( "$1" != mirror && "$1" != private && "$1" != runner && "$1" != publisher && "$1" != release ) ]]; then
    echo "usage: WITCHYC=/path/to/witchyc $0 mirror|private|runner|publisher|release LINUX_WASMTIME_HOST NEW_OUTPUT_DIRECTORY" >&2
    exit 2
fi
role=$1
host=$2
output=$3
deploy=$(cd -- "$(dirname -- "$0")" && pwd -P)
src=$(cd -- "$deploy/../../witchy2" && pwd -P)
compiler=${WITCHYC:-witchyc}
if [[ ! -f "$host" || ! -x "$host" ]]; then
    echo "supply the executable Linux Witchy Wasmtime host from the matching toolchain" >&2
    exit 2
fi
case $(LC_ALL=C file -bL -- "$host") in
    'ELF 64-bit LSB '*aarch64* | 'ELF 64-bit LSB '*x86-64*) ;;
    *) echo "Coven deployment requires a Linux ARM64 or x86-64 ELF host" >&2; exit 2 ;;
esac

hash() {
    if command -v sha384sum >/dev/null 2>&1; then
        sha384sum "$@"
    else
        shasum -a 384 "$@"
    fi
}

mkdir -- "$output"
output=$(cd -- "$output" && pwd -P)
stage=$(mktemp -d "$output/.build.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
tree="$stage/tree"
prefix="$tree/opt/coven"
mkdir -p "$prefix/bin" "$prefix/libexec/coven" "$tree/systemd" "$tree/pins"
install -m 0755 "$host" "$prefix/bin/witchy-wasmtime-host"
install -m 0755 "$src/coven" "$prefix/libexec/coven/coven"
install -m 0644 "$src"/coven_*.py "$src"/witchy_{compiler,release}.py "$prefix/libexec/coven/"
ln -s ../libexec/coven/coven "$prefix/bin/coven"

build() {
    local entry=$1 destination=$2 pages=$3 slots=$4 stack=$5
    "$compiler" wasm-caps --policy "$src/$entry.policy" "$src/$entry.wy"
    "$compiler" build "$src/$entry.wy" -o "$destination" \
        --heap-pages="$pages" --slots="$slots" --stack-bytes="$stack"
    chmod 0644 "$destination"
}

pin() {
    local checksum
    checksum=$(hash "$1")
    printf '%s=sha384:%s\n' "$2" "${checksum%% *}"
}

if [[ "$role" == mirror ]]; then
    install -m 0644 "$deploy"/coven-{mirror.service,public.service,public.socket} "$tree/systemd/"
    build mirror_https "$prefix/libexec/coven-mirror.wasm" 9 64 16384
    build mirror_tls_check "$prefix/libexec/coven-mirror-tls-check.wasm" 48 4096 65536
    build mirror_check "$prefix/libexec/coven-mirror-check.wasm" 48 4096 65536
    pin "$prefix/libexec/coven-mirror-check.wasm" COVEN_MIRROR_CHECK_PIN > "$tree/pins/probe.env"
    {
        pin "$prefix/libexec/coven-mirror.wasm" COVEN_MIRROR_MODULE_PIN
        pin "$prefix/libexec/coven-mirror-tls-check.wasm" COVEN_MIRROR_TLS_CHECK_PIN
    } > "$tree/pins/mirror.env"
elif [[ "$role" == private ]]; then
    install -m 0644 "$deploy"/coven-{ci.service,review.service,private-ingress@.service,private-ingress@ci.socket,private-ingress@review.socket} "$tree/systemd/"
    mkdir -p "$prefix/libexec/coven/release_web" "$prefix/libexec/coven-private"
    install -m 0644 "$src"/release_web/*.{mjs,css} "$prefix/libexec/coven/release_web/"
    modules="$prefix/libexec/coven-private"
    build ci_https "$modules/ci-https.wasm" 32 256 65536
    build ci_identity "$modules/ci-identity.wasm" 128 4096 65536
    build ci_commit "$modules/ci-commit.wasm" 128 4096 65536
    build review_https "$modules/review.wasm" 352 512 65536
    build review_read_challenge "$modules/review-challenge.wasm" 128 65536 65536
    build review_inspect "$modules/review-inspect.wasm" 64 4096 65536
    build review_commit "$modules/review-commit.wasm" 64 4096 65536
    build tuf_review_baseline "$modules/review-baseline.wasm" 448 65536 65536
    build staging_schema "$modules/staging-schema.wasm" 4 64 32768
    build review_set_policy "$modules/review-set-policy.wasm" 128 65536 65536
    build staging_backup_manifest "$modules/staging-backup-manifest.wasm" 16 512 32768
    build staging_recovery "$modules/staging-recovery.wasm" 4 64 32768
    {
        pin "$modules/ci-https.wasm" COVEN_CI_LISTENER_PIN
        pin "$modules/ci-identity.wasm" COVEN_CI_IDENTITY_PIN
        pin "$modules/ci-commit.wasm" COVEN_CI_STORAGE_PIN
    } > "$tree/pins/ci.env"
    {
        pin "$modules/review.wasm" COVEN_REVIEW_LISTENER_PIN
        pin "$modules/review-challenge.wasm" COVEN_REVIEW_CHALLENGE_PIN
        pin "$modules/review-inspect.wasm" COVEN_REVIEW_INSPECT_PIN
        pin "$modules/review-commit.wasm" COVEN_REVIEW_COMMIT_PIN
        pin "$modules/review-baseline.wasm" COVEN_REVIEW_BASELINE_PIN
    } > "$tree/pins/review.env"
    {
        pin "$modules/staging-schema.wasm" COVEN_STAGING_SCHEMA_PIN
        pin "$modules/review-set-policy.wasm" COVEN_REVIEW_POLICY_MODULE_PIN
    } > "$tree/pins/provision.env"
    {
        pin "$modules/staging-backup-manifest.wasm" COVEN_STAGING_BACKUP_MANIFEST_PIN
        pin "$modules/staging-recovery.wasm" COVEN_STAGING_RECOVERY_PIN
    } > "$tree/pins/recovery.env"
elif [[ "$role" == runner ]]; then
    modules="$prefix/libexec/coven-runner"
    mkdir -p "$modules"
    build ci_sign "$modules/ci-sign.wasm" 32 512 65536
    build ci_post "$modules/ci-post.wasm" 48 4096 65536
    {
        pin "$modules/ci-sign.wasm" COVEN_CI_SIGNER_PIN
        pin "$modules/ci-post.wasm" COVEN_CI_TRANSPORT_PIN
    } > "$tree/pins/runner.env"
elif [[ "$role" == publisher ]]; then
    modules="$prefix/libexec/coven-publisher"
    mkdir -p "$modules"
    build tuf_prepare_repository "$modules/tuf-prepare-repository.wasm" 256 4096 65536
    pin "$modules/tuf-prepare-repository.wasm" COVEN_TUF_PUBLICATION_PIN > "$tree/pins/publisher.env"
else
    modules="$prefix/libexec/coven-release"
    mkdir -p "$modules"
    build tuf_publisher_sign "$modules/publisher-sign.wasm" 96 4096 65536
    build tuf_witness_seal "$modules/witness-seal.wasm" 256 4096 65536
    build tuf_seal_snapshot "$modules/seal-snapshot.wasm" 64 4096 65536
    build tuf_delegate_prepare "$modules/delegate-prepare.wasm" 448 65536 65536
    build tuf_delegate_sign "$modules/delegate-sign.wasm" 448 4096 65536
    build tuf_delegate_finalize "$modules/delegate-finalize.wasm" 448 65536 65536
    build tuf_rotate_policy "$modules/rotate-policy.wasm" 256 65536 65536
    {
        pin "$modules/publisher-sign.wasm" COVEN_TUF_PUBLISHER_SIGN_PIN
        pin "$modules/witness-seal.wasm" COVEN_TUF_WITNESS_SEAL_PIN
        pin "$modules/seal-snapshot.wasm" COVEN_TUF_SEAL_SNAPSHOT_PIN
        pin "$modules/delegate-prepare.wasm" COVEN_TUF_DELEGATE_PREPARE_PIN
        pin "$modules/delegate-sign.wasm" COVEN_TUF_DELEGATE_SIGN_PIN
        pin "$modules/delegate-finalize.wasm" COVEN_TUF_DELEGATE_FINALIZE_PIN
        pin "$modules/rotate-policy.wasm" COVEN_TUF_ROTATE_POLICY_PIN
    } > "$tree/pins/release.env"
fi

if [[ "$role" == mirror || "$role" == private ]]; then
    build tls_prepare "$prefix/libexec/coven-tls-prepare.wasm" 48 4096 65536
    pin "$prefix/libexec/coven-tls-prepare.wasm" COVEN_TLS_PREPARE_PIN > "$tree/pins/tls.env"
fi

archive="coven-$role.tar"
COPYFILE_DISABLE=1 tar --format=ustar -cf "$stage/$archive" -C "$tree" opt systemd pins
(cd -- "$stage" && hash "$archive" > SHA384SUMS)
mv -- "$stage/$archive" "$stage/SHA384SUMS" "$output/"
printf '%s\n' "$output/$archive"
