#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <openssl/evp.h>
#include <openssl/nid.h>
extern uint64_t witchy_public_mldsa_verify(uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t);
static void check_suite(uint64_t suite, int nid, size_t key_size, size_t signature_size) {
    uint8_t seed[32] = {0}, pub[2592], sig[4627], empty = 0;
    assert(key_size <= sizeof pub && signature_size <= sizeof sig);
    size_t pub_len = key_size, sig_len = signature_size;
    EVP_PKEY *key = EVP_PKEY_pqdsa_new_raw_private_key(nid, seed, sizeof seed);
    EVP_MD_CTX *ctx = EVP_MD_CTX_new();
    assert(key && ctx);
    assert(EVP_PKEY_get_raw_public_key(key, pub, &pub_len) == 1);
    assert(EVP_DigestSignInit(ctx, NULL, NULL, NULL, key) == 1);
    assert(EVP_DigestSign(ctx, sig, &sig_len, &empty, 0) == 1);
    assert(pub_len == key_size && sig_len == signature_size);
    assert(witchy_public_mldsa_verify(suite, (uintptr_t)pub, pub_len, 0, 0, (uintptr_t)sig, sig_len) == 1);
    sig[0] ^= 1;
    assert(witchy_public_mldsa_verify(suite, (uintptr_t)pub, pub_len, 0, 0, (uintptr_t)sig, sig_len) == 0);
    assert(witchy_public_mldsa_verify(suite, 0, pub_len - 1, 0, 0, 0, sig_len) == 0);
    assert(witchy_public_mldsa_verify(suite, 0, pub_len, 0, 0, 0, sig_len - 1) == 0);
    assert(witchy_public_mldsa_verify(suite, 0, pub_len, 0, (1u << 26) + 1, 0, sig_len) == 0);
    EVP_MD_CTX_free(ctx);
    EVP_PKEY_free(key);
    printf("ML-DSA-%llu ABI: empty message and rejection cases passed\n",
           (unsigned long long)suite);
}

int main(void) {
    check_suite(65, NID_MLDSA65, 1952, 3309);
#ifdef __APPLE__
    check_suite(87, NID_MLDSA87, 2592, 4627);
#endif
    assert(witchy_public_mldsa_verify(0, 0, 0, 0, 0, 0, 0) == 0);
    return 0;
}
