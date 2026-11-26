#include <assert.h>
#include <stdint.h>
#include <string.h>

extern uint64_t witchy_tls13_aead(uint64_t mode, const uint8_t *key, size_t key_len,
                                 const uint8_t *nonce, size_t nonce_len,
                                 const uint8_t *header, size_t header_len,
                                 const uint8_t *input, size_t input_len,
                                 uint8_t *output, size_t output_len);

extern uint64_t witchy_verify_tag48(const void *left, size_t left_len,
                                    const void *right, size_t right_len);

static void check_tag(void) {
    uint8_t left[49] = {0}, right[49] = {0};
    right[48] = 1;
    assert(witchy_verify_tag48(left, 48, right, 48) == 1);
    right[47] = 1;
    assert(witchy_verify_tag48(left, 48, right, 48) == 0);
    assert(witchy_verify_tag48(NULL, 47, NULL, 48) == 0);
    assert(witchy_verify_tag48(NULL, 48, NULL, 49) == 0);
}

static void check_record(uint64_t mode, size_t key_len, size_t length) {
    const uint8_t key[32] = {0}, nonce[12] = {(uint8_t)length};
    const uint8_t header[5] = {23, 3, 3, 0, (uint8_t)(length + 16)};
    const uint8_t input[] = {1, 2, 3};
    uint8_t sealed[20], opened[4];
    memset(sealed, 0xA5, sizeof(sealed));
    memset(opened, 0xA5, sizeof(opened));

    assert(witchy_tls13_aead(mode, key, key_len, nonce, sizeof(nonce), header, sizeof(header),
                             length ? input : NULL, length, sealed, length + 16) == 0);
    assert(sealed[length + 16] == 0xA5);
    assert(witchy_tls13_aead(mode + 1, key, key_len, nonce, sizeof(nonce), header, sizeof(header),
                             sealed, length + 16, length ? opened : NULL, length) == 0);
    assert(memcmp(input, opened, length) == 0);
    assert(opened[length] == 0xA5);

    sealed[length + 15] ^= 1;
    memset(opened, 0xA5, sizeof(opened));
    assert(witchy_tls13_aead(mode + 1, key, key_len, nonce, sizeof(nonce), header, sizeof(header),
                             sealed, length + 16, length ? opened : NULL, length) == 4);
    for (size_t i = 0; i < sizeof(opened); ++i) assert(opened[i] == 0xA5);
}

static void check_overlapping_record(void) {
    const uint8_t key[16] = {9}, nonce[12] = {0};
    const uint8_t header[5] = {23, 3, 3, 0, 19};
    uint8_t record[20] = {1, 2, 3};
    record[19] = 0xA5;
    assert(witchy_tls13_aead(0, key, sizeof(key), nonce, sizeof(nonce), header, sizeof(header),
                             record, 3, record, 19) == 0);
    assert(record[19] == 0xA5);

    uint8_t unchanged[sizeof(record)];
    memcpy(unchanged, record, sizeof(record));
    record[18] ^= 1;
    unchanged[18] ^= 1;
    assert(witchy_tls13_aead(1, key, sizeof(key), nonce, sizeof(nonce), header, sizeof(header),
                             record, 19, record, 3) == 4);
    assert(memcmp(record, unchanged, sizeof(record)) == 0);

    record[18] ^= 1;
    unchanged[18] ^= 1;
    assert(witchy_tls13_aead(1, key, sizeof(key), nonce, sizeof(nonce), header, sizeof(header),
                             record, 19, record, 3) == 0);
    assert(memcmp(record, "\x01\x02\x03", 3) == 0);
    assert(memcmp(record + 3, unchanged + 3, sizeof(record) - 3) == 0);
}

static void check_maximum_record(void) {
    enum { MAX_CIPHERTEXT = (1 << 14) + 256, MAX_INPUT = MAX_CIPHERTEXT - 16 };
    const uint8_t key[32] = {7}, nonce[12] = {0};
    const uint8_t header[5] = {23, 3, 3, MAX_CIPHERTEXT >> 8, MAX_CIPHERTEXT & 255};
    uint8_t input[MAX_INPUT], sealed[MAX_CIPHERTEXT + 1], opened[MAX_INPUT + 1];
    memset(input, 0x42, sizeof(input));
    memset(sealed, 0xA5, sizeof(sealed));
    memset(opened, 0xA5, sizeof(opened));
    assert(witchy_tls13_aead(2, key, sizeof(key), nonce, sizeof(nonce), header, sizeof(header),
                             input, sizeof(input), sealed, MAX_CIPHERTEXT) == 0);
    assert(sealed[MAX_CIPHERTEXT] == 0xA5);
    assert(witchy_tls13_aead(3, key, sizeof(key), nonce, sizeof(nonce), header, sizeof(header),
                             sealed, MAX_CIPHERTEXT, opened, MAX_INPUT) == 0);
    assert(memcmp(input, opened, sizeof(input)) == 0 && opened[MAX_INPUT] == 0xA5);
    assert(witchy_tls13_aead(2, NULL, 0, NULL, 0, NULL, 0, NULL, 0, NULL,
                             MAX_CIPHERTEXT + 1) == 3);
}

int main(void) {
    assert(witchy_tls13_aead(0, NULL, 0, NULL, 0, NULL, 0, NULL, 0, NULL, 16) == 2);
    assert(witchy_tls13_aead(0, NULL, 0, NULL, 0, NULL, 0, NULL, SIZE_MAX, NULL, 0) == 3);
    check_record(0, 16, 0);
    check_record(0, 16, 3);
    check_record(2, 32, 0);
    check_record(2, 32, 3);
    check_tag();
    check_maximum_record();
    check_overlapping_record();
    return 0;
}
