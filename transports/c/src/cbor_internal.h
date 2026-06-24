/* Minimal canonical CBOR codec (RFC 8949), internal to the transport library.
 *
 * Like the Go reference's lowercase (package-private) cbor.go, this surface is not
 * installed: only the .c sources include it. It supports exactly what CSIL
 * envelopes need - unsigned ints, negative ints, text strings, byte strings,
 * arrays, maps, and tag 24 - and nothing else. Maps are encoded with core
 * deterministic encoding (entries sorted by the bytewise order of their encoded
 * keys), so the same logical envelope always yields the conformance-vector bytes
 * regardless of C struct layout. */
#ifndef CSIL_CBOR_INTERNAL_H
#define CSIL_CBOR_INTERNAL_H

#include "csil/csil_conventions.h"

/* ---- growable byte buffer (encode side) ----------------------------------
 * All growth funnels through realloc so OOM is one check. The final data
 * pointer is plain malloc'd memory, handed to the caller and freed via
 * csil_free, so encode never leaks an intermediate codec type. */
typedef struct csil_buf {
    uint8_t *data;
    size_t len;
    size_t cap;
} csil_buf;

void csil_buf_init(csil_buf *b);
void csil_buf_dispose(csil_buf *b);
csil_err csil_buf_push(csil_buf *b, uint8_t byte);
csil_err csil_buf_append(csil_buf *b, const uint8_t *p, size_t n);

/* ---- CBOR write primitives (append to a csil_buf) -------------------------
 * Big-endian argument bytes are emitted MSB-first with explicit shifts on a
 * uint64_t, so the codec is endianness-agnostic and needs no htonl/arpa. */
csil_err csil_cbor_write_uint(csil_buf *b, uint64_t n);
csil_err csil_cbor_write_int(csil_buf *b, int64_t x);
csil_err csil_cbor_write_text(csil_buf *b, const char *s, size_t n);
csil_err csil_cbor_write_bytes(csil_buf *b, const uint8_t *p, size_t n);
csil_err csil_cbor_write_array_head(csil_buf *b, uint64_t count);
/* Wrap opaque payload bytes (themselves a CBOR item) in tag 24. */
csil_err csil_cbor_write_payload(csil_buf *b, const uint8_t *p, size_t n);

/* ---- canonical map builder ------------------------------------------------
 * Each add encodes its value into a private fragment; finish sorts the entries
 * by the bytewise order of their encoded keys and emits the map. Because the
 * envelope keys are all text strings, the length-prefixed encoding makes the
 * sort "shorter-first, then bytewise", matching RFC 8949 sec 4.2.1. */
typedef struct csil_map_kv {
    csil_buf key;
    csil_buf val;
} csil_map_kv;

typedef struct csil_map_builder {
    csil_map_kv *entries;
    size_t count;
    size_t cap;
    csil_err err; /* sticky: the first allocation failure is reported by finish */
} csil_map_builder;

void csil_map_init(csil_map_builder *m);
void csil_map_dispose(csil_map_builder *m);
void csil_map_add_uint(csil_map_builder *m, const char *key, uint64_t v);
void csil_map_add_int(csil_map_builder *m, const char *key, int64_t v);
void csil_map_add_text(csil_map_builder *m, const char *key, const char *v);
void csil_map_add_payload(csil_map_builder *m, const char *key,
                          const uint8_t *p, size_t n);
void csil_map_add_uint_array(csil_map_builder *m, const char *key,
                             const uint64_t *vals, size_t n);
void csil_map_add_text_array(csil_map_builder *m, const char *key,
                             const char *const *vals, size_t n);
/* Sort and emit into out (which the caller initialized). Returns the sticky
 * error if any add ran out of memory. */
csil_err csil_map_finish(csil_map_builder *m, csil_buf *out);

/* ---- bump arena (decode side) --------------------------------------------
 * A decoded value tree is a recursive tagged union of unknown arity, so one
 * arena backs the whole tree: every node, string, and byte slice points into
 * it, and csil_arena_free releases it once - no deep recursive free, no
 * dangling sub-pointers. */
typedef struct csil_arena csil_arena;
csil_arena *csil_arena_new(void);
void *csil_arena_alloc(csil_arena *a, size_t size);
void csil_arena_free(csil_arena *a);

/* ---- CBOR value tree (decode side) ---------------------------------------- */
typedef enum csil_cbor_kind {
    CSIL_CBOR_UINT,
    CSIL_CBOR_NINT,
    CSIL_CBOR_TEXT,
    CSIL_CBOR_BYTES,
    CSIL_CBOR_ARRAY,
    CSIL_CBOR_MAP,
    CSIL_CBOR_TAG
} csil_cbor_kind;

typedef struct csil_cbor_value csil_cbor_value;
typedef struct csil_cbor_pair {
    csil_cbor_value *key;
    csil_cbor_value *val;
} csil_cbor_pair;

struct csil_cbor_value {
    csil_cbor_kind kind;
    union {
        uint64_t u; /* UINT */
        int64_t i;  /* NINT */
        struct {    /* TEXT (NUL-terminated in arena) and BYTES */
            const uint8_t *ptr;
            size_t len;
        } bytes;
        struct {
            csil_cbor_value *items;
            size_t count;
        } array;
        struct {
            csil_cbor_pair *pairs;
            size_t count;
        } map;
        struct {
            uint64_t num;
            csil_cbor_value *content;
        } tag;
    } as;
};

/* Decode a complete envelope: one self-contained CBOR item with no trailing
 * bytes (conventions doc sec 1). Allocates the value tree in *out_arena, which
 * the caller frees with csil_arena_free once it is done with *out. */
csil_err csil_cbor_decode_envelope(const uint8_t *b, size_t len,
                                   csil_arena **out_arena,
                                   csil_cbor_value **out);

/* ---- decoded-tree accessors (used by the envelope decoders) --------------- */
const csil_cbor_value *csil_cbor_map_get(const csil_cbor_value *v, const char *key);
bool csil_cbor_as_u64(const csil_cbor_value *v, uint64_t *out);
bool csil_cbor_as_i64(const csil_cbor_value *v, int64_t *out);
/* Extract the opaque payload bytes from a tag-24 value (arena-borrowed). */
csil_err csil_cbor_untag24(const csil_cbor_value *v, const uint8_t **out,
                           size_t *out_len);

#endif /* CSIL_CBOR_INTERNAL_H */
