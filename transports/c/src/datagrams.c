#include "csil/csil_datagrams.h"

#include "envelope_internal.h"

#include <stdlib.h>
#include <string.h>

/* ---- CBOR-array profile --------------------------------------------------- */

void csil_datagram_init(csil_datagram *d, uint64_t op_ord, uint64_t seq,
                        const uint8_t *payload, size_t payload_len) {
    memset(d, 0, sizeof(*d));
    d->op_ord = op_ord;
    d->seq = seq;
    d->payload = payload;
    d->payload_len = payload_len;
}

csil_err csil_datagram_encode(const csil_datagram *d, uint8_t **out,
                              size_t *out_len) {
    csil_buf b;
    csil_buf_init(&b);
    csil_err e = csil_cbor_write_array_head(&b, 4);
    if (!e) {
        e = csil_cbor_write_uint(&b, CSIL_VERSION);
    }
    if (!e) {
        e = csil_cbor_write_uint(&b, d->op_ord);
    }
    if (!e) {
        e = csil_cbor_write_uint(&b, d->seq);
    }
    if (!e) {
        e = csil_cbor_write_payload(&b, d->payload, d->payload_len);
    }
    if (e) {
        csil_buf_dispose(&b);
        return e;
    }
    *out = b.data;
    *out_len = b.len;
    return CSIL_OK;
}

csil_err csil_datagram_decode(const uint8_t *frame, size_t len,
                              csil_datagram *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    if (v->kind != CSIL_CBOR_ARRAY || v->as.array.count != 4) {
        csil_arena_free(a);
        return CSIL_ERR_MALFORMED;
    }
    const csil_cbor_value *items = v->as.array.items;
    uint64_t ver = 0, op_ord = 0, seq = 0;
    if (!csil_cbor_as_u64(&items[0], &ver)) {
        csil_arena_free(a);
        return CSIL_ERR_MALFORMED;
    }
    if ((e = env_check_version(ver))) {
        csil_arena_free(a);
        return e;
    }
    if (!csil_cbor_as_u64(&items[1], &op_ord) ||
        !csil_cbor_as_u64(&items[2], &seq)) {
        csil_arena_free(a);
        return CSIL_ERR_MALFORMED;
    }
    memset(out, 0, sizeof(*out));
    if ((e = csil_cbor_untag24(&items[3], &out->payload, &out->payload_len))) {
        csil_arena_free(a);
        return e;
    }
    out->op_ord = op_ord;
    out->seq = seq;
    out->_arena = a;
    return CSIL_OK;
}

void csil_datagram_free(csil_datagram *d) {
    if (d && d->_arena) {
        csil_arena_free(d->_arena);
        d->_arena = NULL;
    }
}

bool csil_datagram_equal(const csil_datagram *a, const csil_datagram *b) {
    return a->op_ord == b->op_ord && a->seq == b->seq &&
           env_bytes_eq(a->payload, a->payload_len, b->payload, b->payload_len);
}

/* ---- compact fixed-header profile ----------------------------------------- */

#define CSIL_COMPACT_VER ((uint8_t)1)
#define CSIL_FLAG_EPOCH ((uint8_t)0x01)

void csil_compact_datagram_init(csil_compact_datagram *d, uint8_t op_ord,
                                uint16_t seq, const uint8_t *body,
                                size_t body_len) {
    memset(d, 0, sizeof(*d));
    d->op_ord = op_ord;
    d->seq = seq;
    d->body = body;
    d->body_len = body_len;
}

void csil_compact_datagram_set_epoch(csil_compact_datagram *d, uint8_t epoch) {
    d->has_epoch = true;
    d->epoch = epoch;
}

csil_err csil_compact_datagram_encode(const csil_compact_datagram *d, uint8_t **out,
                                      size_t *out_len) {
    size_t header = d->has_epoch ? 5 : 4;
    size_t total = header + d->body_len;
    uint8_t *buf = malloc(total ? total : 1);
    if (!buf) {
        return CSIL_ERR_OOM;
    }
    uint8_t flags = d->has_epoch ? CSIL_FLAG_EPOCH : 0;
    buf[0] = (uint8_t)((CSIL_COMPACT_VER << 4) | (flags & 0x0f));
    buf[1] = d->op_ord;
    buf[2] = (uint8_t)(d->seq >> 8);
    buf[3] = (uint8_t)d->seq;
    size_t off = 4;
    if (d->has_epoch) {
        buf[4] = d->epoch;
        off = 5;
    }
    if (d->body_len) {
        memcpy(buf + off, d->body, d->body_len);
    }
    *out = buf;
    *out_len = total;
    return CSIL_OK;
}

csil_err csil_compact_datagram_decode(const uint8_t *frame, size_t len,
                                      csil_compact_datagram *out) {
    if (len < 4) {
        return CSIL_ERR_MALFORMED;
    }
    uint8_t ver = frame[0] >> 4;
    if (ver != CSIL_COMPACT_VER) {
        return CSIL_ERR_UNSUPPORTED_VERSION;
    }
    uint8_t flags = frame[0] & 0x0f;
    memset(out, 0, sizeof(*out));
    out->op_ord = frame[1];
    out->seq = (uint16_t)(((uint16_t)frame[2] << 8) | (uint16_t)frame[3]);
    size_t body_start = 4;
    if (flags & CSIL_FLAG_EPOCH) {
        if (len < 5) {
            return CSIL_ERR_MALFORMED;
        }
        out->has_epoch = true;
        out->epoch = frame[4];
        body_start = 5;
    }
    size_t body_len = len - body_start;
    uint8_t *owned = malloc(body_len ? body_len : 1);
    if (!owned) {
        return CSIL_ERR_OOM;
    }
    if (body_len) {
        memcpy(owned, frame + body_start, body_len);
    }
    out->_owned = owned;
    out->body = owned;
    out->body_len = body_len;
    return CSIL_OK;
}

void csil_compact_datagram_free(csil_compact_datagram *d) {
    if (d && d->_owned) {
        free(d->_owned);
        d->_owned = NULL;
        d->body = NULL;
    }
}

bool csil_compact_datagram_equal(const csil_compact_datagram *a,
                                 const csil_compact_datagram *b) {
    return a->op_ord == b->op_ord && a->seq == b->seq &&
           a->has_epoch == b->has_epoch &&
           (!a->has_epoch || a->epoch == b->epoch) &&
           env_bytes_eq(a->body, a->body_len, b->body, b->body_len);
}

/* ---- sequence tracker ----------------------------------------------------- */

void csil_seq_tracker_init(csil_seq_tracker *t) { memset(t, 0, sizeof(*t)); }

static bool epoch_equal(bool a_has, uint8_t a, bool b_has, uint8_t b) {
    if (!a_has || !b_has) {
        return !a_has && !b_has;
    }
    return a == b;
}

csil_seq_event csil_seq_tracker_observe(csil_seq_tracker *t, uint64_t seq,
                                        bool has_epoch, uint8_t epoch) {
    csil_seq_event ev = {CSIL_SEQ_FIRST, 0};
    if (!epoch_equal(has_epoch, epoch, t->has_last_epoch, t->last_epoch) &&
        t->has_last_epoch) {
        t->has_last_epoch = has_epoch;
        t->last_epoch = epoch;
        t->has_last_seq = true;
        t->last_seq = seq;
        ev.kind = CSIL_SEQ_RESTART;
        return ev;
    }
    t->has_last_epoch = has_epoch;
    t->last_epoch = epoch;
    /* seq 0 marks an unsequenced datagram: it carries no ordering information, so
     * it is never late or duplicate. Report a zero-gap advance and leave the
     * running sequence untouched. */
    if (seq == 0) {
        ev.kind = CSIL_SEQ_ADVANCED;
        ev.gap = 0;
        return ev;
    }
    if (!t->has_last_seq) {
        t->has_last_seq = true;
        t->last_seq = seq;
        ev.kind = CSIL_SEQ_FIRST;
        return ev;
    }
    if (seq > t->last_seq) {
        ev.kind = CSIL_SEQ_ADVANCED;
        ev.gap = seq - t->last_seq - 1;
        t->last_seq = seq;
        return ev;
    }
    ev.kind = CSIL_SEQ_LATE_OR_DUPLICATE;
    return ev;
}
