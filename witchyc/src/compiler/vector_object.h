#ifndef WITCHY_SIMD_H
#define WITCHY_SIMD_H

typedef unsigned long long V __attribute__((vector_size(16)));

/* Host vector ABI kernels emitted by build-object. */
enum Width { BYTE, HALF, WORD, DOUBLE };
#define INLINE static inline __attribute__((always_inline))

#if defined(__aarch64__) && defined(__ARM_NEON)
#define NEON(op, suffix)                                                                           \
    do {                                                                                           \
        V r;                                                                                       \
        __asm__(op " %0." suffix ", %1." suffix ", %2." suffix : "=w"(r) : "w"(a), "w"(b));        \
        return r;                                                                                  \
    } while (0)
#define LANES(op)                                                                                  \
    switch (w) {                                                                                   \
    case BYTE:                                                                                     \
        NEON(op, "16b");                                                                           \
    case HALF:                                                                                     \
        NEON(op, "8h");                                                                            \
    case WORD:                                                                                     \
        NEON(op, "4s");                                                                            \
    default:                                                                                       \
        NEON(op, "2d");                                                                            \
    }

INLINE V add(enum Width w, V a, V b){LANES("add")} INLINE V
    sub(enum Width w, V a, V b){LANES("sub")} INLINE V
    equal(enum Width w, V a, V b){LANES("cmeq")} INLINE V
    greater(enum Width w, V a, V b){LANES("cmhi")} INLINE V bit_and(V a, V b) {
    NEON("and", "16b");
}
INLINE V bit_or(V a, V b) { NEON("orr", "16b"); }
INLINE V bit_xor(V a, V b) { NEON("eor", "16b"); }

INLINE V multiply(enum Width w, V a, V b) {
    switch (w) {
    case BYTE:
        NEON("mul", "16b");
    case HALF:
        NEON("mul", "8h");
    case WORD:
        NEON("mul", "4s");
    default: {
        /* lo*lo + ((hi*lo + lo*hi) << 32), independently in each lane. */
        V loA, loB, hiA, hiB, result, cross;
        __asm__("xtn %0.2s, %1.2d" : "=w"(loA) : "w"(a));
        __asm__("xtn %0.2s, %1.2d" : "=w"(loB) : "w"(b));
        __asm__("shrn %0.2s, %1.2d, #32" : "=w"(hiA) : "w"(a));
        __asm__("shrn %0.2s, %1.2d, #32" : "=w"(hiB) : "w"(b));
        __asm__("umull %0.2d, %1.2s, %2.2s" : "=w"(result) : "w"(loA), "w"(loB));
        __asm__("mul %0.2s, %1.2s, %2.2s" : "=w"(cross) : "w"(hiA), "w"(loB));
        __asm__("mla %0.2s, %1.2s, %2.2s" : "+w"(cross) : "w"(loA), "w"(hiB));
        __asm__("shll %0.2d, %1.2s, #32" : "=w"(cross) : "w"(cross));
        return add(DOUBLE, result, cross);
    }
    }
}

#elif defined(__x86_64__) && defined(__SSE2__)
#include <emmintrin.h>
#define SSE(op)                                                                                    \
    do {                                                                                           \
        __asm__(op " %1, %0" : "+x"(a) : "x"(b));                                                  \
        return a;                                                                                  \
    } while (0)
#define LANES(op)                                                                                  \
    switch (w) {                                                                                   \
    case BYTE:                                                                                     \
        SSE(op "b");                                                                               \
    case HALF:                                                                                     \
        SSE(op "w");                                                                               \
    case WORD:                                                                                     \
        SSE(op "d");                                                                               \
    default:                                                                                       \
        SSE(op "q");                                                                               \
    }

INLINE V add(enum Width w, V a, V b){LANES("padd")} INLINE V
    sub(enum Width w, V a, V b){LANES("psub")} INLINE V bit_and(V a, V b) {
    SSE("pand");
}
INLINE V bit_or(V a, V b) { SSE("por"); }
INLINE V bit_xor(V a, V b) { SSE("pxor"); }

INLINE V equal(enum Width w, V a, V b) {
    switch (w) {
    case BYTE:
        SSE("pcmpeqb");
    case HALF:
        SSE("pcmpeqw");
    case WORD:
        SSE("pcmpeqd");
    default: {
        __m128i eq = _mm_cmpeq_epi32((__m128i)a, (__m128i)b);
        return (V)_mm_and_si128(eq, _mm_shuffle_epi32(eq, _MM_SHUFFLE(2, 3, 0, 1)));
    }
    }
}

INLINE V greater(enum Width w, V a, V b) {
    /* Bias each unsigned lane so the signed SSE2 comparison preserves order. */
    const unsigned long long biases[] = {0x8080808080808080ULL, 0x8000800080008000ULL,
                                         0x8000000080000000ULL, 0x8000000080000000ULL};
    V bias = (V){biases[w], biases[w]};
    a = bit_xor(a, bias);
    b = bit_xor(b, bias);
    switch (w) {
    case BYTE:
        SSE("pcmpgtb");
    case HALF:
        SSE("pcmpgtw");
    case WORD:
        SSE("pcmpgtd");
    default: {
        __m128i gt = _mm_cmpgt_epi32((__m128i)a, (__m128i)b);
        __m128i eq = _mm_cmpeq_epi32((__m128i)a, (__m128i)b);
        __m128i hi = _mm_shuffle_epi32(gt, _MM_SHUFFLE(3, 3, 1, 1));
        __m128i tie = _mm_shuffle_epi32(eq, _MM_SHUFFLE(3, 3, 1, 1));
        __m128i lo = _mm_shuffle_epi32(gt, _MM_SHUFFLE(2, 2, 0, 0));
        return (V)_mm_or_si128(hi, _mm_and_si128(tie, lo));
    }
    }
}

INLINE V multiply(enum Width w, V a, V b) {
    __m128i x = (__m128i)a, y = (__m128i)b;
    if (w == HALF) {
        SSE("pmullw");
    }
    if (w == BYTE) {
        __m128i low = _mm_and_si128(_mm_mullo_epi16(x, y), _mm_set1_epi16(255));
        __m128i high = _mm_mullo_epi16(_mm_srli_epi16(x, 8), _mm_srli_epi16(y, 8));
        return (V)_mm_or_si128(low, _mm_slli_epi16(high, 8));
    }
    __m128i lo = _mm_mul_epu32(x, y);
    __m128i xhi = _mm_srli_epi64(x, 32), yhi = _mm_srli_epi64(y, 32);
    if (w == WORD) {
        __m128i hi = _mm_mul_epu32(xhi, yhi);
        return (V)_mm_or_si128(_mm_srli_epi64(_mm_slli_epi64(lo, 32), 32), _mm_slli_epi64(hi, 32));
    }
    __m128i cross = _mm_add_epi64(_mm_mul_epu32(xhi, y), _mm_mul_epu32(x, yhi));
    return (V)_mm_add_epi64(lo, _mm_slli_epi64(cross, 32));
}
#else
#error "vector objects require AArch64 Advanced SIMD or x86-64 SSE2"
#endif
#endif
