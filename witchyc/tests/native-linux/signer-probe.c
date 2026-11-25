// The Linux signer with the AWS-LC FIPS module, below the language (programs cannot hold the
// 1952- and 3309-byte buffers without the proved Vec, which needs the proof checker this
// container lacks): the seed from descriptor 3 only, the module in FIPS mode, the public key of
// NIST's ACVP ML-DSA-65 key-generation vector (shown by its SHA-384, as tests/fixtures/crypto/
// sign.wy), and a signature made after the guest's own sandbox is installed that verifies with
// the public key alone; a message outside the role's domain is refused.
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <openssl/crypto.h>
#include <openssl/evp.h>
#include <openssl/nid.h>
bool witchy_bind_signer(const char *binding);

const unsigned char witchy_secret_slots[] = {3, 't', 'a', 'r', 'g', 'e', 't', 's', 0, 0};
uint64_t witchy_signer_public(uint64_t handle, uint64_t output, uint64_t capacity);
uint64_t witchy_signer_sign(uint64_t handle, uint64_t message, uint64_t length, uint64_t output, uint64_t capacity);
bool witchy_install_sandbox(bool output, bool allocator);
void witchy_sandbox_allow_signing(void);

int main(void) {
    if (witchy_bind_signer("targets:sign/witchy-2/targets:fd:4"))
        return puts("FAIL a seed from an unopened descriptor was bound"), 1;
    if (!witchy_bind_signer("targets:sign/witchy-2/targets:fd:3"))
        return puts("FAIL the seed was not bound"), 1;
    printf("fips %d\n", FIPS_mode());
    static unsigned char key[1952], signature[3309], digest[48];
    if (witchy_signer_public(18, (uintptr_t)key, sizeof key) != 0)
        return puts("FAIL no public key"), 1;
    unsigned digest_length = 0;
    EVP_Digest(key, sizeof key, digest, &digest_length, EVP_sha384(), NULL);
    for (unsigned i = 0; i < digest_length; ++i)
        printf("%02x", digest[i]);
    puts("");
    fflush(stdout);
    witchy_sandbox_allow_signing();
    if (!witchy_install_sandbox(true, true))
        return puts("FAIL sandbox"), 1;
    const char message[] = "witchy/v2/targets\0release 1\n";
    printf("sign %d\n", (int)witchy_signer_sign(18, (uintptr_t)message, sizeof message - 1, (uintptr_t)signature, sizeof signature));
    EVP_PKEY *public_key = EVP_PKEY_pqdsa_new_raw_public_key(NID_MLDSA65, key, sizeof key);
    EVP_MD_CTX *ctx = EVP_MD_CTX_new();
    int valid = public_key && ctx && EVP_DigestVerifyInit(ctx, NULL, NULL, NULL, public_key) == 1 &&
        EVP_DigestVerify(ctx, signature, sizeof signature, (const uint8_t *)message, sizeof message - 1) == 1;
    printf("verify %d\n", valid);
    const char other[] = "witchy/v2/root\0release 1\n";
    printf("other role %d\n", (int)witchy_signer_sign(18, (uintptr_t)other, sizeof other - 1, (uintptr_t)signature, sizeof signature));
    fflush(stdout);
    return 0;
}
