#include "cbor_internal.h"

#include <string.h>
#include <stdlib.h>

/* ---- growable buffer ------------------------------------------------------ */

void csil_buf_init(csil_buf *b) {
    b->data = NULL;
    b->len = 0;
    b->cap = 0;
}

void csil_buf_dispose(csil_buf *b) {
    free(b->data);
    b->data = NULL;
    b->len = 0;
    b->cap = 0;
}

static csil_err csil_buf_reserve(csil_buf *b, size_t extra) {
    /* Guard the addition itself: a hostile length must not wrap size_t into a
     * too-small capacity and then overrun. */
    if (extra > SIZE_MAX - b->len) {
        return CSIL_ERR_OOM;
    }
    size_t need = b->len + extra;
    if (need <= b->cap) {
        return CSIL_OK;
    }
    size_t cap = b->cap ? b->cap : 64;
    while (cap < need) {
        if (cap > SIZE_MAX / 2) {
            cap = need;
            break;
        }
        cap *= 2;
    }
    uint8_t *p = realloc(b->data, cap);
    if (!p) {
        return CSIL_ERR_OOM; /* old buffer still valid and owned by b */
    }
    b->data = p;
    b->cap = cap;
    return CSIL_OK;
}

csil_err csil_buf_push(csil_buf *b, uint8_t byte) {
    csil_err e = csil_buf_reserve(b, 1);
    if (e) {
        return e;
    }
    b->data[b->len++] = byte;
    return CSIL_OK;
}

csil_err csil_buf_append(csil_buf *b, const uint8_t *p, size_t n) {
    if (n == 0) {
        return CSIL_OK;
    }
    csil_err e = csil_buf_reserve(b, n);
    if (e) {
        return e;
    }
    memcpy(b->data + b->len, p, n);
    b->len += n;
    return CSIL_OK;
}

/* ---- write primitives ----------------------------------------------------- */

/* Emit the initial byte (major type in the high three bits) plus the
 * shortest-form argument bytes for n, per deterministic encoding. */
static csil_err cbor_head(csil_buf *b, uint8_t major, uint64_t n) {
    uint8_t mt = (uint8_t)(major << 5);
    if (n < 24) {
        return csil_buf_push(b, (uint8_t)(mt | (uint8_t)n));
    }
    if (n < 0x100ULL) {
        uint8_t t[2] = {(uint8_t)(mt | 24), (uint8_t)n};
        return csil_buf_append(b, t, 2);
    }
    if (n < 0x10000ULL) {
        uint8_t t[3] = {(uint8_t)(mt | 25), (uint8_t)(n >> 8), (uint8_t)n};
        return csil_buf_append(b, t, 3);
    }
    if (n < 0x100000000ULL) {
        uint8_t t[5] = {(uint8_t)(mt | 26), (uint8_t)(n >> 24), (uint8_t)(n >> 16),
                        (uint8_t)(n >> 8), (uint8_t)n};
        return csil_buf_append(b, t, 5);
    }
    uint8_t t[9] = {(uint8_t)(mt | 27),     (uint8_t)(n >> 56), (uint8_t)(n >> 48),
                    (uint8_t)(n >> 40),     (uint8_t)(n >> 32), (uint8_t)(n >> 24),
                    (uint8_t)(n >> 16),     (uint8_t)(n >> 8),  (uint8_t)n};
    return csil_buf_append(b, t, 9);
}

csil_err csil_cbor_write_uint(csil_buf *b, uint64_t n) {
    return cbor_head(b, 0, n);
}

csil_err csil_cbor_write_int(csil_buf *b, int64_t x) {
    if (x >= 0) {
        return cbor_head(b, 0, (uint64_t)x);
    }
    /* CBOR negative ints encode -1-x; the magnitude is computed in unsigned to
     * stay defined even at INT64_MIN. */
    uint64_t mag = (uint64_t)(-(x + 1));
    return cbor_head(b, 1, mag);
}

csil_err csil_cbor_write_text(csil_buf *b, const char *s, size_t n) {
    csil_err e = cbor_head(b, 3, (uint64_t)n);
    if (e) {
        return e;
    }
    return csil_buf_append(b, (const uint8_t *)s, n);
}

csil_err csil_cbor_write_bytes(csil_buf *b, const uint8_t *p, size_t n) {
    csil_err e = cbor_head(b, 2, (uint64_t)n);
    if (e) {
        return e;
    }
    return csil_buf_append(b, p, n);
}

csil_err csil_cbor_write_array_head(csil_buf *b, uint64_t count) {
    return cbor_head(b, 4, count);
}

csil_err csil_cbor_write_payload(csil_buf *b, const uint8_t *p, size_t n) {
    csil_err e = cbor_head(b, 6, CSIL_TAG_ENCODED_CBOR);
    if (e) {
        return e;
    }
    return csil_cbor_write_bytes(b, p, n);
}

/* ---- canonical map builder ------------------------------------------------ */

void csil_map_init(csil_map_builder *m) {
    m->entries = NULL;
    m->count = 0;
    m->cap = 0;
    m->err = CSIL_OK;
}

void csil_map_dispose(csil_map_builder *m) {
    for (size_t i = 0; i < m->count; i++) {
        csil_buf_dispose(&m->entries[i].key);
        csil_buf_dispose(&m->entries[i].val);
    }
    free(m->entries);
    m->entries = NULL;
    m->count = 0;
    m->cap = 0;
}

/* Encode the text key into kv->key; the value fragment is filled by the caller. */
static csil_map_kv *map_begin(csil_map_builder *m, const char *key) {
    if (m->err) {
        return NULL;
    }
    if (m->count == m->cap) {
        size_t cap = m->cap ? m->cap * 2 : 8;
        csil_map_kv *p = realloc(m->entries, cap * sizeof(*p));
        if (!p) {
            m->err = CSIL_ERR_OOM;
            return NULL;
        }
        m->entries = p;
        m->cap = cap;
    }
    csil_map_kv *kv = &m->entries[m->count];
    csil_buf_init(&kv->key);
    csil_buf_init(&kv->val);
    csil_err e = csil_cbor_write_text(&kv->key, key, strlen(key));
    if (e) {
        csil_buf_dispose(&kv->key);
        m->err = e;
        return NULL;
    }
    return kv;
}

static void map_commit(csil_map_builder *m, csil_map_kv *kv) {
    if (m->err) {
        csil_buf_dispose(&kv->key);
        csil_buf_dispose(&kv->val);
        return;
    }
    m->count++;
}

void csil_map_add_uint(csil_map_builder *m, const char *key, uint64_t v) {
    csil_map_kv *kv = map_begin(m, key);
    if (!kv) {
        return;
    }
    m->err = csil_cbor_write_uint(&kv->val, v);
    map_commit(m, kv);
}

void csil_map_add_int(csil_map_builder *m, const char *key, int64_t v) {
    csil_map_kv *kv = map_begin(m, key);
    if (!kv) {
        return;
    }
    m->err = csil_cbor_write_int(&kv->val, v);
    map_commit(m, kv);
}

void csil_map_add_text(csil_map_builder *m, const char *key, const char *v) {
    csil_map_kv *kv = map_begin(m, key);
    if (!kv) {
        return;
    }
    m->err = csil_cbor_write_text(&kv->val, v, strlen(v));
    map_commit(m, kv);
}

void csil_map_add_payload(csil_map_builder *m, const char *key, const uint8_t *p,
                          size_t n) {
    csil_map_kv *kv = map_begin(m, key);
    if (!kv) {
        return;
    }
    m->err = csil_cbor_write_payload(&kv->val, p, n);
    map_commit(m, kv);
}

void csil_map_add_uint_array(csil_map_builder *m, const char *key,
                             const uint64_t *vals, size_t n) {
    csil_map_kv *kv = map_begin(m, key);
    if (!kv) {
        return;
    }
    csil_err e = csil_cbor_write_array_head(&kv->val, n);
    for (size_t i = 0; e == CSIL_OK && i < n; i++) {
        e = csil_cbor_write_uint(&kv->val, vals[i]);
    }
    m->err = e;
    map_commit(m, kv);
}

void csil_map_add_text_array(csil_map_builder *m, const char *key,
                             const char *const *vals, size_t n) {
    csil_map_kv *kv = map_begin(m, key);
    if (!kv) {
        return;
    }
    csil_err e = csil_cbor_write_array_head(&kv->val, n);
    for (size_t i = 0; e == CSIL_OK && i < n; i++) {
        e = csil_cbor_write_text(&kv->val, vals[i], strlen(vals[i]));
    }
    m->err = e;
    map_commit(m, kv);
}

static int kv_cmp(const void *a, const void *b) {
    const csil_map_kv *ka = a;
    const csil_map_kv *kb = b;
    size_t n = ka->key.len < kb->key.len ? ka->key.len : kb->key.len;
    int c = n ? memcmp(ka->key.data, kb->key.data, n) : 0;
    if (c != 0) {
        return c;
    }
    if (ka->key.len < kb->key.len) {
        return -1;
    }
    if (ka->key.len > kb->key.len) {
        return 1;
    }
    return 0;
}

csil_err csil_map_finish(csil_map_builder *m, csil_buf *out) {
    if (m->err) {
        return m->err;
    }
    /* Deterministic encoding: order entries by the bytewise order of their
     * encoded keys before emitting (RFC 8949 sec 4.2.1). qsort is not stable,
     * but envelope keys are unique so order is fully determined by the key. */
    qsort(m->entries, m->count, sizeof(*m->entries), kv_cmp);
    csil_err e = cbor_head(out, 5, (uint64_t)m->count);
    for (size_t i = 0; e == CSIL_OK && i < m->count; i++) {
        e = csil_buf_append(out, m->entries[i].key.data, m->entries[i].key.len);
        if (e == CSIL_OK) {
            e = csil_buf_append(out, m->entries[i].val.data, m->entries[i].val.len);
        }
    }
    return e;
}

/* ---- bump arena ----------------------------------------------------------- */

typedef struct csil_arena_block {
    struct csil_arena_block *next;
    size_t used;
    size_t cap;
    /* data follows in the same allocation */
} csil_arena_block;

struct csil_arena {
    csil_arena_block *head;
};

#define CSIL_ARENA_BLOCK_MIN ((size_t)4096)
#define CSIL_ARENA_ALIGN ((size_t)16)

csil_arena *csil_arena_new(void) {
    csil_arena *a = calloc(1, sizeof(*a));
    return a;
}

static uint8_t *block_data(csil_arena_block *blk) {
    return (uint8_t *)blk + sizeof(csil_arena_block);
}

void *csil_arena_alloc(csil_arena *a, size_t size) {
    if (!a) {
        return NULL;
    }
    size_t aligned = (size + (CSIL_ARENA_ALIGN - 1)) & ~(CSIL_ARENA_ALIGN - 1);
    if (aligned < size) {
        return NULL; /* overflow on rounding */
    }
    csil_arena_block *blk = a->head;
    if (!blk || blk->cap - blk->used < aligned) {
        size_t cap = aligned > CSIL_ARENA_BLOCK_MIN ? aligned : CSIL_ARENA_BLOCK_MIN;
        if (cap > SIZE_MAX - sizeof(csil_arena_block)) {
            return NULL;
        }
        blk = malloc(sizeof(csil_arena_block) + cap);
        if (!blk) {
            return NULL;
        }
        blk->used = 0;
        blk->cap = cap;
        blk->next = a->head;
        a->head = blk;
    }
    void *p = block_data(blk) + blk->used;
    blk->used += aligned;
    return p;
}

void csil_arena_free(csil_arena *a) {
    if (!a) {
        return;
    }
    csil_arena_block *blk = a->head;
    while (blk) {
        csil_arena_block *next = blk->next;
        free(blk);
        blk = next;
    }
    free(a);
}

/* ---- decode --------------------------------------------------------------- */

/* Read the additional-information argument for a head byte whose low five bits
 * are low, returning the argument value and the total head length. */
static csil_err read_arg(const uint8_t *b, size_t len, uint8_t low, uint64_t *arg,
                         size_t *head_len) {
    if (low < 24) {
        *arg = low;
        *head_len = 1;
        return CSIL_OK;
    }
    if (low == 24) {
        if (len < 2) {
            return CSIL_ERR_TRUNCATED;
        }
        *arg = b[1];
        *head_len = 2;
        return CSIL_OK;
    }
    if (low == 25) {
        if (len < 3) {
            return CSIL_ERR_TRUNCATED;
        }
        *arg = ((uint64_t)b[1] << 8) | (uint64_t)b[2];
        *head_len = 3;
        return CSIL_OK;
    }
    if (low == 26) {
        if (len < 5) {
            return CSIL_ERR_TRUNCATED;
        }
        *arg = ((uint64_t)b[1] << 24) | ((uint64_t)b[2] << 16) |
               ((uint64_t)b[3] << 8) | (uint64_t)b[4];
        *head_len = 5;
        return CSIL_OK;
    }
    if (low == 27) {
        if (len < 9) {
            return CSIL_ERR_TRUNCATED;
        }
        uint64_t v = 0;
        for (int i = 1; i <= 8; i++) {
            v = (v << 8) | (uint64_t)b[i];
        }
        *arg = v;
        *head_len = 9;
        return CSIL_OK;
    }
    /* 28..31 (indefinite-length / reserved) are forbidden in CSIL envelopes. */
    return CSIL_ERR_UNSUPPORTED_TYPE;
}

/* Copy n bytes from src into the arena, NUL-terminating when as_text so the
 * result is usable directly as a C string. */
static const uint8_t *arena_copy(csil_arena *a, const uint8_t *src, size_t n,
                                 bool as_text) {
    if (as_text && n == SIZE_MAX) {
        return NULL;
    }
    size_t total = as_text ? n + 1 : (n ? n : 1);
    uint8_t *dst = csil_arena_alloc(a, total);
    if (!dst) {
        return NULL;
    }
    if (n) {
        memcpy(dst, src, n);
    }
    if (as_text) {
        dst[n] = 0;
    }
    return dst;
}

static bool valid_utf8(const uint8_t *s, size_t n) {
    size_t i = 0;
    while (i < n) {
        uint8_t c = s[i++];
        if (c < 0x80) continue;
        size_t need;
        uint32_t cp;
        if (c >= 0xc2 && c <= 0xdf) { need = 1; cp = c & 0x1f; }
        else if (c >= 0xe0 && c <= 0xef) { need = 2; cp = c & 0x0f; }
        else if (c >= 0xf0 && c <= 0xf4) { need = 3; cp = c & 0x07; }
        else return false;
        if (need > n - i) return false;
        for (size_t j = 0; j < need; j++) {
            uint8_t d = s[i++];
            if ((d & 0xc0) != 0x80) return false;
            cp = (cp << 6) | (uint32_t)(d & 0x3f);
        }
        if ((need == 2 && cp < 0x800) || (need == 3 && cp < 0x10000) ||
            (cp >= 0xd800 && cp <= 0xdfff) || cp > 0x10ffff) return false;
    }
    return true;
}

static csil_err decode_value(csil_arena *a, const uint8_t *b, size_t len,
                             csil_cbor_value *out, size_t *consumed, size_t depth) {
    if (depth > 64) {
        return CSIL_ERR_MALFORMED;
    }
    if (len == 0) {
        return CSIL_ERR_TRUNCATED;
    }
    uint8_t ib = b[0];
    uint8_t major = ib >> 5;
    uint8_t low = ib & 0x1f;
    uint64_t arg = 0;
    size_t head = 0;
    csil_err e = read_arg(b, len, low, &arg, &head);
    if (e) {
        return e;
    }
    switch (major) {
    case 0:
        out->kind = CSIL_CBOR_UINT;
        out->as.u = arg;
        *consumed = head;
        return CSIL_OK;
    case 1:
        /* An arg above INT64_MAX names a value below INT64_MIN, which would
         * silently wrap if forced into an int64. */
        if (arg > (uint64_t)INT64_MAX) {
            return CSIL_ERR_MALFORMED;
        }
        out->kind = CSIL_CBOR_NINT;
        out->as.i = -1 - (int64_t)arg;
        *consumed = head;
        return CSIL_OK;
    case 2:
    case 3: {
        if (arg > len - head) {
            return CSIL_ERR_TRUNCATED;
        }
        bool as_text = major == 3;
        if (as_text && !valid_utf8(b + head, (size_t)arg)) {
            return CSIL_ERR_MALFORMED;
        }
        const uint8_t *copy = arena_copy(a, b + head, (size_t)arg, as_text);
        if (!copy) {
            return CSIL_ERR_OOM;
        }
        out->kind = as_text ? CSIL_CBOR_TEXT : CSIL_CBOR_BYTES;
        out->as.bytes.ptr = copy;
        out->as.bytes.len = (size_t)arg;
        *consumed = head + (size_t)arg;
        return CSIL_OK;
    }
    case 4: {
        if (arg > len - head || arg > SIZE_MAX / sizeof(csil_cbor_value)) {
            return CSIL_ERR_TRUNCATED;
        }
        csil_cbor_value *items = NULL;
        if (arg) {
            items = csil_arena_alloc(a, (size_t)arg * sizeof(*items));
            if (!items) {
                return CSIL_ERR_OOM;
            }
        }
        size_t off = head;
        for (uint64_t i = 0; i < arg; i++) {
            size_t m = 0;
            e = decode_value(a, b + off, len - off, &items[i], &m, depth + 1);
            if (e) {
                return e;
            }
            off += m;
        }
        out->kind = CSIL_CBOR_ARRAY;
        out->as.array.items = items;
        out->as.array.count = (size_t)arg;
        *consumed = off;
        return CSIL_OK;
    }
    case 5: {
        if (arg > len - head || arg > SIZE_MAX / sizeof(csil_cbor_pair)) {
            return CSIL_ERR_TRUNCATED;
        }
        csil_cbor_pair *pairs = NULL;
        if (arg) {
            pairs = csil_arena_alloc(a, (size_t)arg * sizeof(*pairs));
            if (!pairs) {
                return CSIL_ERR_OOM;
            }
        }
        size_t off = head;
        for (uint64_t i = 0; i < arg; i++) {
            csil_cbor_value *k = csil_arena_alloc(a, sizeof(*k));
            csil_cbor_value *v = csil_arena_alloc(a, sizeof(*v));
            if (!k || !v) {
                return CSIL_ERR_OOM;
            }
            size_t m = 0;
            e = decode_value(a, b + off, len - off, k, &m, depth + 1);
            if (e) {
                return e;
            }
            off += m;
            e = decode_value(a, b + off, len - off, v, &m, depth + 1);
            if (e) {
                return e;
            }
            off += m;
            pairs[i].key = k;
            pairs[i].val = v;
        }
        out->kind = CSIL_CBOR_MAP;
        out->as.map.pairs = pairs;
        out->as.map.count = (size_t)arg;
        *consumed = off;
        return CSIL_OK;
    }
    case 6: {
        csil_cbor_value *content = csil_arena_alloc(a, sizeof(*content));
        if (!content) {
            return CSIL_ERR_OOM;
        }
        size_t m = 0;
        e = decode_value(a, b + head, len - head, content, &m, depth + 1);
        if (e) {
            return e;
        }
        out->kind = CSIL_CBOR_TAG;
        out->as.tag.num = arg;
        out->as.tag.content = content;
        *consumed = head + m;
        return CSIL_OK;
    }
    default:
        return CSIL_ERR_UNSUPPORTED_TYPE;
    }
}

csil_err csil_cbor_decode_envelope(const uint8_t *b, size_t len,
                                   csil_arena **out_arena, csil_cbor_value **out) {
    *out_arena = NULL;
    *out = NULL;
    csil_arena *a = csil_arena_new();
    if (!a) {
        return CSIL_ERR_OOM;
    }
    csil_cbor_value *root = csil_arena_alloc(a, sizeof(*root));
    if (!root) {
        csil_arena_free(a);
        return CSIL_ERR_OOM;
    }
    size_t consumed = 0;
    csil_err e = decode_value(a, b, len, root, &consumed, 0);
    if (e) {
        csil_arena_free(a);
        return e;
    }
    /* An envelope is a single CBOR item; leftover bytes are a malformed frame. */
    if (consumed != len) {
        csil_arena_free(a);
        return CSIL_ERR_TRAILING;
    }
    *out_arena = a;
    *out = root;
    return CSIL_OK;
}

/* ---- decoded-tree accessors ----------------------------------------------- */

const csil_cbor_value *csil_cbor_map_get(const csil_cbor_value *v,
                                         const char *key) {
    if (!v || v->kind != CSIL_CBOR_MAP) {
        return NULL;
    }
    size_t klen = strlen(key);
    for (size_t i = 0; i < v->as.map.count; i++) {
        const csil_cbor_value *k = v->as.map.pairs[i].key;
        if (k->kind == CSIL_CBOR_TEXT && k->as.bytes.len == klen &&
            memcmp(k->as.bytes.ptr, key, klen) == 0) {
            return v->as.map.pairs[i].val;
        }
    }
    return NULL;
}

bool csil_cbor_as_u64(const csil_cbor_value *v, uint64_t *out) {
    if (!v) {
        return false;
    }
    if (v->kind == CSIL_CBOR_UINT) {
        *out = v->as.u;
        return true;
    }
    if (v->kind == CSIL_CBOR_NINT && v->as.i >= 0) {
        *out = (uint64_t)v->as.i;
        return true;
    }
    return false;
}

bool csil_cbor_as_i64(const csil_cbor_value *v, int64_t *out) {
    if (!v) {
        return false;
    }
    if (v->kind == CSIL_CBOR_UINT) {
        if (v->as.u > (uint64_t)INT64_MAX) {
            return false;
        }
        *out = (int64_t)v->as.u;
        return true;
    }
    if (v->kind == CSIL_CBOR_NINT) {
        *out = v->as.i;
        return true;
    }
    return false;
}

csil_err csil_cbor_untag24(const csil_cbor_value *v, const uint8_t **out,
                           size_t *out_len) {
    if (!v || v->kind != CSIL_CBOR_TAG || v->as.tag.num != CSIL_TAG_ENCODED_CBOR) {
        return CSIL_ERR_MALFORMED;
    }
    const csil_cbor_value *c = v->as.tag.content;
    if (c->kind != CSIL_CBOR_BYTES) {
        return CSIL_ERR_MALFORMED;
    }
    *out = c->as.bytes.ptr;
    *out_len = c->as.bytes.len;
    return CSIL_OK;
}
