#include <assert.h>
#include <stdint.h>
#include <string.h>

extern uint64_t witchy_p384_start(uint64_t grant);
extern uint64_t witchy_p384_close(uint64_t handle);
extern uint64_t witchy_p384_public(uint64_t handle, uint8_t *output, size_t length);
extern uint64_t witchy_p384_finish(uint64_t handle, const uint8_t *peer, size_t peer_len,
                                  uint8_t *output, size_t length);
extern uint64_t witchy_p384_agree(uint64_t grant, const uint8_t *peer, size_t peer_len,
                                 uint8_t *public, size_t public_len,
                                 uint8_t *secret, size_t secret_len);

enum { RAND_GRANT = 32, POINT = 97, SECRET = 48, SENTINEL = 0xA5 };

static void check_slot_reuse(void) {
    uint64_t handles[16];
    for (size_t i = 0; i < 16; ++i) {
        handles[i] = witchy_p384_start(RAND_GRANT);
        assert(handles[i] != 0);
    }
    assert(witchy_p384_start(RAND_GRANT) == 0);
    assert(witchy_p384_close(handles[0]) == 0);
    uint64_t replacement = witchy_p384_start(RAND_GRANT);
    assert(replacement != 0 && replacement != handles[0]);

    uint8_t output[POINT];
    memset(output, SENTINEL, sizeof(output));
    assert(witchy_p384_public(handles[0], output, POINT) == 4);
    for (size_t i = 0; i < sizeof(output); ++i) assert(output[i] == SENTINEL);
    assert(witchy_p384_public(replacement, output, POINT) == 0);
    assert(witchy_p384_close(replacement) == 0);
    for (size_t i = 1; i < 16; ++i) assert(witchy_p384_close(handles[i]) == 0);
}

int main(void) {
    uint64_t key = witchy_p384_start(RAND_GRANT);
    assert(key != 0);
    assert(witchy_p384_public(key, NULL, POINT - 1) == 2);
    uint8_t peer[POINT + 1];
    memset(peer, SENTINEL, sizeof(peer));
    assert(witchy_p384_public(key, peer, POINT) == 0);
    assert(peer[0] == 4 && peer[POINT] == SENTINEL);

    uint8_t public[POINT + 1], secret[SECRET + 1], comparison[SECRET + 1];
    memset(public, SENTINEL, sizeof(public));
    memset(secret, SENTINEL, sizeof(secret));
    memset(comparison, SENTINEL, sizeof(comparison));
    assert(witchy_p384_agree(RAND_GRANT, NULL, 0, NULL, POINT, NULL, SECRET) == 2);
    uint8_t invalid_peer[POINT] = {0};
    assert(witchy_p384_agree(RAND_GRANT, invalid_peer, POINT, public, POINT, secret, SECRET) == 1);
    for (size_t i = 0; i < sizeof(public); ++i) assert(public[i] == SENTINEL);
    for (size_t i = 0; i < sizeof(secret); ++i) assert(secret[i] == SENTINEL);

    uint64_t rejected = witchy_p384_start(RAND_GRANT);
    assert(rejected != 0);
    assert(witchy_p384_finish(rejected, invalid_peer, POINT, comparison, SECRET) == 1);
    for (size_t i = 0; i < sizeof(comparison); ++i) assert(comparison[i] == SENTINEL);
    assert(witchy_p384_public(rejected, public, POINT) == 4);
    for (size_t i = 0; i < sizeof(public); ++i) assert(public[i] == SENTINEL);

    assert(witchy_p384_agree(RAND_GRANT, peer, POINT, public, POINT, secret, SECRET) == 0);
    assert(witchy_p384_finish(key, public, POINT, comparison, SECRET) == 0);
    assert(memcmp(secret, comparison, SECRET) == 0);
    assert(public[POINT] == SENTINEL && secret[SECRET] == SENTINEL && comparison[SECRET] == SENTINEL);
    check_slot_reuse();
    return 0;
}
