/* tinyjump C ABI. Strings are UTF-8 with explicit lengths. All calls on one runtime
 * must come from one thread. Status codes: 0 = outputs JSON array, 1 = compile errors
 * or bad input, 2 = boot error (see docs/host-protocol.md). */
#ifndef TINYJUMP_H
#define TINYJUMP_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* A Rust-owned buffer that callbacks fill with tj_buf_set (the bytes are copied). */
typedef struct TjBuf TjBuf;
void tj_buf_set(TjBuf *buf, const uint8_t *data, size_t len);

typedef struct TjStore {
    void *ctx;
    /* Fill `out` and return 1 if the key exists, else return 0. */
    int (*load)(void *ctx, const uint8_t *key, size_t key_len, TjBuf *out);
    /* Return 0 on success. */
    int (*save)(void *ctx, const uint8_t *key, size_t key_len, const uint8_t *value, size_t value_len);
    /* Fill `out` with a JSON array of the keys that start with `prefix`. */
    void (*keys)(void *ctx, const uint8_t *prefix, size_t prefix_len, TjBuf *out);
} TjStore;

typedef struct TjRuntime TjRuntime;

TjRuntime *tj_new(TjStore store);
int tj_boot(TjRuntime *rt, const uint8_t *source, size_t len, double now_ms, uint64_t seed, int render);
int tj_handle(TjRuntime *rt, double now_ms, const uint8_t *input, size_t len);
/* The JSON payload of the last call; valid until the next call on this runtime. */
const uint8_t *tj_out(const TjRuntime *rt, size_t *len);
void tj_free(TjRuntime *rt);

#ifdef __cplusplus
}
#endif

#endif
