#include <openssl/aead.h>
#include <openssl/rand.h>
#include <openssl/bytestring.h>
#include <openssl/evp.h>
#include <openssl/crypto.h>
#include <openssl/sha.h>
#include <openssl/ec.h>
#include <openssl/ec_key.h>
#include <openssl/ecdh.h>
#include <openssl/mem.h>
#include <openssl/nid.h>
#include <openssl/hmac.h>
#include <openssl/err.h>
#include <openssl/x509.h>

#define TYPE(S, T) _Static_assert(__builtin_types_compatible_p(__typeof__(S), T), #S)
TYPE(&EC_KEY_new_by_curve_name, EC_KEY *(*)(int));
TYPE(&EC_KEY_free, void (*)(EC_KEY *));
TYPE(&EC_KEY_generate_key_fips, int (*)(EC_KEY *));
TYPE(&EC_KEY_get0_public_key, const EC_POINT *(*)(const EC_KEY *));
TYPE(&EC_POINT_new, EC_POINT *(*)(const EC_GROUP *));
TYPE(&EC_POINT_free, void (*)(EC_POINT *));
TYPE(&EC_POINT_oct2point, int (*)(const EC_GROUP *, EC_POINT *, const uint8_t *, size_t, BN_CTX *));
TYPE(&EC_POINT_point2oct, size_t (*)(const EC_GROUP *, const EC_POINT *, point_conversion_form_t, uint8_t *, size_t, BN_CTX *));
typedef void *(*Kdf)(const void *, size_t, void *, size_t *);
TYPE(&ECDH_compute_key, int (*)(void *, size_t, const EC_POINT *, const EC_KEY *, Kdf));
TYPE(&OPENSSL_cleanse, void (*)(void *, size_t));
_Static_assert(sizeof(point_conversion_form_t) == sizeof(int), "point form width");
_Static_assert(POINT_CONVERSION_UNCOMPRESSED == 4, "uncompressed form");

TYPE(&EVP_aead_aes_128_gcm_tls13, const EVP_AEAD *(*)(void));
TYPE(&EVP_aead_aes_256_gcm_tls13, const EVP_AEAD *(*)(void));
TYPE(&EVP_AEAD_CTX_new, EVP_AEAD_CTX *(*)(const EVP_AEAD *, const uint8_t *, size_t, size_t));
TYPE(&EVP_AEAD_CTX_free, void (*)(EVP_AEAD_CTX *));
typedef int (*AeadTransform)(const EVP_AEAD_CTX *, uint8_t *, size_t *, size_t,
                             const uint8_t *, size_t, const uint8_t *, size_t,
                             const uint8_t *, size_t);
TYPE(&EVP_AEAD_CTX_seal, AeadTransform);
TYPE(&EVP_AEAD_CTX_open, AeadTransform);
TYPE(&CRYPTO_memcmp, int (*)(const void *, const void *, size_t));

TYPE(&FIPS_mode, int (*)(void));
TYPE(&RAND_bytes, int (*)(uint8_t *, size_t));
TYPE(&SHA256, uint8_t *(*)(const uint8_t *, size_t, uint8_t *));
TYPE(&SHA384, uint8_t *(*)(const uint8_t *, size_t, uint8_t *));
TYPE(&SHA512, uint8_t *(*)(const uint8_t *, size_t, uint8_t *));
_Static_assert(SHA256_DIGEST_LENGTH == 32 && SHA384_DIGEST_LENGTH == 48
               && SHA512_DIGEST_LENGTH == 64, "digest sizes");

_Static_assert(sizeof(CBS) == 16 && _Alignof(CBS) == 8, "CBS layout");
_Static_assert(offsetof(CBS, data) == 0 && offsetof(CBS, len) == 8, "CBS offsets");
TYPE(&EVP_parse_private_key, EVP_PKEY *(*)(CBS *));
TYPE(&EVP_PKEY_free, void (*)(EVP_PKEY *));
TYPE(&EC_KEY_check_fips, int (*)(const EC_KEY *));
TYPE(&EVP_DigestSignInit, int (*)(EVP_MD_CTX *, EVP_PKEY_CTX **, const EVP_MD *, ENGINE *, EVP_PKEY *));
TYPE(&EVP_DigestSign, int (*)(EVP_MD_CTX *, uint8_t *, size_t *, const uint8_t *, size_t));
TYPE(&EVP_PKEY_pqdsa_new_raw_private_key, EVP_PKEY *(*)(int, const uint8_t *, size_t));
TYPE(&EVP_PKEY_pqdsa_new_raw_public_key, EVP_PKEY *(*)(int, const uint8_t *, size_t));
_Static_assert(NID_MLDSA65 == 995 && NID_MLDSA87 == 996, "ML-DSA provider identifiers");
TYPE(&EVP_PKEY_get_raw_public_key, int (*)(const EVP_PKEY *, uint8_t *, size_t *));

TYPE(&HMAC_CTX_new, HMAC_CTX *(*)(void));
TYPE(&HMAC_CTX_free, void (*)(HMAC_CTX *));
TYPE(&HMAC_Init_ex, int (*)(HMAC_CTX *, const void *, size_t, const EVP_MD *, ENGINE *));
TYPE(&HMAC_Update, int (*)(HMAC_CTX *, const uint8_t *, size_t));
TYPE(&HMAC_Final, int (*)(HMAC_CTX *, uint8_t *, unsigned int *));
TYPE(&EVP_sha256, const EVP_MD *(*)(void));
_Static_assert(sizeof(unsigned int) == sizeof(uint32_t), "HMAC output length width");

TYPE(&d2i_X509, X509 *(*)(X509 **, const unsigned char **, long));
TYPE(&X509_free, void (*)(X509 *));
TYPE(&X509_STORE_new, X509_STORE *(*)(void));
TYPE(&X509_STORE_free, void (*)(X509_STORE *));
TYPE(&X509_STORE_add_cert, int (*)(X509_STORE *, X509 *));
TYPE(&OPENSSL_sk_new_null, _STACK *(*)(void));
TYPE(&OPENSSL_sk_free, void (*)(_STACK *));
TYPE(&OPENSSL_sk_push, size_t (*)(_STACK *, void *));
TYPE(&OPENSSL_sk_num, size_t (*)(const _STACK *));
TYPE(&OPENSSL_sk_value, void *(*)(const _STACK *, size_t));
TYPE(&X509_STORE_CTX_new, X509_STORE_CTX *(*)(void));
TYPE(&X509_STORE_CTX_free, void (*)(X509_STORE_CTX *));
TYPE(&X509_STORE_CTX_init, int (*)(X509_STORE_CTX *, X509_STORE *, X509 *, STACK_OF(X509) *));
TYPE(&X509_STORE_CTX_set_purpose, int (*)(X509_STORE_CTX *, int));
TYPE(&X509_STORE_CTX_get0_param, X509_VERIFY_PARAM *(*)(X509_STORE_CTX *));
TYPE(&X509_VERIFY_PARAM_set_time_posix, void (*)(X509_VERIFY_PARAM *, int64_t));
TYPE(&X509_VERIFY_PARAM_set_hostflags, void (*)(X509_VERIFY_PARAM *, unsigned int));
TYPE(&X509_VERIFY_PARAM_set1_host, int (*)(X509_VERIFY_PARAM *, const char *, size_t));
TYPE(&X509_verify_cert, int (*)(X509_STORE_CTX *));
TYPE(&X509_STORE_CTX_get0_chain, STACK_OF(X509) *(*)(X509_STORE_CTX *));
TYPE(&X509_get_key_usage, uint32_t (*)(X509 *));
TYPE(&X509_get0_pubkey, EVP_PKEY *(*)(const X509 *));
TYPE(&X509_get_signature_info, int (*)(X509 *, int *, int *, int *, uint32_t *));
TYPE(&EVP_PKEY_id, int (*)(const EVP_PKEY *));
TYPE(&EVP_PKEY_bits, int (*)(const EVP_PKEY *));
TYPE(&EVP_PKEY_get0_EC_KEY, EC_KEY *(*)(const EVP_PKEY *));
TYPE(&EC_KEY_get0_group, const EC_GROUP *(*)(const EC_KEY *));
TYPE(&EC_GROUP_get_curve_name, int (*)(const EC_GROUP *));
TYPE(&EVP_MD_CTX_new, EVP_MD_CTX *(*)(void));
TYPE(&EVP_MD_CTX_free, void (*)(EVP_MD_CTX *));
TYPE(&EVP_sha384, const EVP_MD *(*)(void));
TYPE(&EVP_DigestVerifyInit, int (*)(EVP_MD_CTX *, EVP_PKEY_CTX **, const EVP_MD *, ENGINE *, EVP_PKEY *));
TYPE(&EVP_DigestVerify, int (*)(EVP_MD_CTX *, const uint8_t *, size_t, const uint8_t *, size_t));
TYPE(&ERR_clear_error, void (*)(void));
