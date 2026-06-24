/* Typed-field accessors shared by the three envelope decoders. They pull fields
 * out of a decoded CBOR map tree; text pointers borrow the arena's
 * NUL-terminated copies, so a decoded struct can point straight at them. */
#ifndef CSIL_ENVELOPE_INTERNAL_H
#define CSIL_ENVELOPE_INTERNAL_H

#include "cbor_internal.h"

#include <string.h>

static inline csil_err env_check_version(uint64_t v) {
    return v == CSIL_VERSION ? CSIL_OK : CSIL_ERR_UNSUPPORTED_VERSION;
}

static inline csil_err env_get_uint(const csil_cbor_value *m, const char *key,
                                    uint64_t *out) {
    const csil_cbor_value *v = csil_cbor_map_get(m, key);
    if (!v || !csil_cbor_as_u64(v, out)) {
        return CSIL_ERR_MALFORMED;
    }
    return CSIL_OK;
}

static inline csil_err env_get_int(const csil_cbor_value *m, const char *key,
                                   int64_t *out) {
    const csil_cbor_value *v = csil_cbor_map_get(m, key);
    if (!v || !csil_cbor_as_i64(v, out)) {
        return CSIL_ERR_MALFORMED;
    }
    return CSIL_OK;
}

static inline csil_err env_get_text(const csil_cbor_value *m, const char *key,
                                    const char **out) {
    const csil_cbor_value *v = csil_cbor_map_get(m, key);
    if (!v || v->kind != CSIL_CBOR_TEXT) {
        return CSIL_ERR_MALFORMED;
    }
    *out = (const char *)v->as.bytes.ptr;
    return CSIL_OK;
}

/* Optional text: NULL when absent or not text. */
static inline const char *env_get_text_opt(const csil_cbor_value *m,
                                           const char *key) {
    const csil_cbor_value *v = csil_cbor_map_get(m, key);
    if (v && v->kind == CSIL_CBOR_TEXT) {
        return (const char *)v->as.bytes.ptr;
    }
    return NULL;
}

/* Optional uint: writes *out and returns true when present and non-negative. */
static inline bool env_get_uint_opt(const csil_cbor_value *m, const char *key,
                                    uint64_t *out) {
    const csil_cbor_value *v = csil_cbor_map_get(m, key);
    return v && csil_cbor_as_u64(v, out);
}

/* Compare two optional C strings (NULL == absent) by content. */
static inline bool env_str_eq(const char *a, const char *b) {
    if (a == NULL || b == NULL) {
        return a == b;
    }
    return strcmp(a, b) == 0;
}

/* Compare two payload byte ranges by content (the decode equality gotcha: a
 * byte-for-byte compare, never a pointer compare). */
static inline bool env_bytes_eq(const uint8_t *a, size_t an, const uint8_t *b,
                                size_t bn) {
    if (an != bn) {
        return false;
    }
    return an == 0 || memcmp(a, b, an) == 0;
}

#endif /* CSIL_ENVELOPE_INTERNAL_H */
