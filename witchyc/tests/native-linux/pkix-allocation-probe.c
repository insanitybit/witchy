#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <openssl/x509.h>
#include <openssl/evp.h>
#include <openssl/stack.h>
static unsigned fail_at;
static unsigned calls;
static void *live[4];
static unsigned live_count;
#define WRAP(T, N, F) \
  T *aws_lc_fips_0_14_4_##N(void); \
  void aws_lc_fips_0_14_4_##F(T *); \
  T *__wrap_aws_lc_fips_0_14_4_##N(void) { \
    if (++calls == fail_at) return NULL; \
    T *ptr = aws_lc_fips_0_14_4_##N(); \
    if (ptr != NULL) { \
      assert(live_count < sizeof live / sizeof live[0]); \
      live[live_count++] = ptr; \
    } \
    return ptr; \
  } \
  void __wrap_aws_lc_fips_0_14_4_##F(T *ptr) { \
    if (ptr == NULL) return; \
    bool found = false; \
    for (unsigned i = 0; i < live_count; ++i) { \
      if (live[i] == ptr) { \
        live[i] = live[--live_count]; \
        found = true; \
        break; \
      } \
    } \
    assert(found); \
    aws_lc_fips_0_14_4_##F(ptr); \
  }
WRAP(X509_STORE, X509_STORE_new, X509_STORE_free)
WRAP(_STACK, OPENSSL_sk_new_null, OPENSSL_sk_free)
WRAP(X509_STORE_CTX, X509_STORE_CTX_new, X509_STORE_CTX_free)
WRAP(EVP_MD_CTX, EVP_MD_CTX_new, EVP_MD_CTX_free)
// Build from witchyc/ so the assembler can find the known-answer inputs.
#define EMBED(NAME, FILE) \
    __asm__(".pushsection .rodata\n" #NAME ":\n" \
            ".incbin \"tests/fixtures/crypto/pkix-kat/" FILE "\"\n" \
            #NAME "_end:\n.popsection")
EMBED(root, "root.bundle");
EMBED(leaf, "leaf.bundle");
EMBED(message, "signed.bin");
EMBED(signature, "signature.der");

extern const unsigned char root[], root_end[], leaf[], leaf_end[];
extern const unsigned char message[], message_end[], signature[], signature_end[];
extern uint64_t witchy_pkix_verify(const void *, size_t, const void *, size_t,
                                  const void *, size_t, const void *, size_t,
                                  const void *, size_t);
static uint64_t verify(void) {
    return witchy_pkix_verify(root, (uintptr_t)root_end - (uintptr_t)root,
                              leaf, (uintptr_t)leaf_end - (uintptr_t)leaf,
                              "api.example.test", 16,
                              message, (uintptr_t)message_end - (uintptr_t)message,
                              signature, (uintptr_t)signature_end - (uintptr_t)signature);
}
int main(void) {
    assert(verify() == 1);
    assert(calls == 4 && live_count == 0);
    for (unsigned index = 1; index <= 4; index++) {
        calls = 0;
        fail_at = index;
        assert(verify() == 0);
        assert(calls == index && live_count == 0);

        calls = 0;
        fail_at = 0;
        assert(verify() == 1);
        assert(calls == 4 && live_count == 0);
    }
    puts("PKIX rejected and recovered after all four constructor failures");
    return 0;
}
