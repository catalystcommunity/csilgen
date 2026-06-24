#include "csil/csil_events.h"

#include "envelope_internal.h"

#include <stdlib.h>
#include <string.h>

static csil_err finish_encode(csil_buf *b, csil_err e, uint8_t **out,
                              size_t *out_len) {
    if (e) {
        csil_buf_dispose(b);
        return e;
    }
    *out = b->data;
    *out_len = b->len;
    return CSIL_OK;
}

const char *csil_profile_name(csil_profile p) {
    switch (p) {
    case CSIL_PROFILE_VERBOSE:
        return "verbose";
    case CSIL_PROFILE_COMPACT:
        return "compact";
    default:
        return "unknown";
    }
}

bool csil_profile_parse(const char *s, csil_profile *out) {
    if (strcmp(s, "verbose") == 0) {
        *out = CSIL_PROFILE_VERBOSE;
        return true;
    }
    if (strcmp(s, "compact") == 0) {
        *out = CSIL_PROFILE_COMPACT;
        return true;
    }
    return false;
}

/* ---- event ---------------------------------------------------------------- */

void csil_event_init_verbose(csil_event *e, const char *service, const char *event,
                             const uint8_t *payload, size_t payload_len) {
    memset(e, 0, sizeof(*e));
    e->service = service;
    e->event = event;
    e->payload = payload;
    e->payload_len = payload_len;
}

void csil_event_init_compact(csil_event *e, uint64_t service_ord, uint64_t op_ord,
                             const uint8_t *payload, size_t payload_len) {
    memset(e, 0, sizeof(*e));
    e->has_service_ord = true;
    e->service_ord = service_ord;
    e->has_op_ord = true;
    e->op_ord = op_ord;
    e->payload = payload;
    e->payload_len = payload_len;
}

static csil_err event_encode_verbose(const csil_event *e, uint8_t **out,
                                     size_t *out_len) {
    if (!e->event) {
        return CSIL_ERR_MALFORMED;
    }
    csil_map_builder m;
    csil_map_init(&m);
    csil_map_add_text(&m, "event", e->event);
    csil_map_add_payload(&m, "payload", e->payload, e->payload_len);
    if (e->service) {
        csil_map_add_text(&m, "service", e->service);
    }
    if (e->has_id) {
        csil_map_add_uint(&m, "id", e->id);
    }
    csil_buf b;
    csil_buf_init(&b);
    csil_err err = csil_map_finish(&m, &b);
    csil_map_dispose(&m);
    return finish_encode(&b, err, out, out_len);
}

static csil_err event_encode_compact(const csil_event *e, uint8_t **out,
                                     size_t *out_len) {
    if (!e->has_service_ord || !e->has_op_ord) {
        return CSIL_ERR_MALFORMED;
    }
    csil_buf b;
    csil_buf_init(&b);
    uint64_t count = e->has_id ? 4 : 3;
    csil_err err = csil_cbor_write_array_head(&b, count);
    if (!err) {
        err = csil_cbor_write_uint(&b, e->service_ord);
    }
    if (!err) {
        err = csil_cbor_write_uint(&b, e->op_ord);
    }
    if (!err && e->has_id) {
        err = csil_cbor_write_uint(&b, e->id);
    }
    if (!err) {
        err = csil_cbor_write_payload(&b, e->payload, e->payload_len);
    }
    return finish_encode(&b, err, out, out_len);
}

csil_err csil_event_encode(const csil_event *e, csil_profile profile, uint8_t **out,
                           size_t *out_len) {
    if (profile == CSIL_PROFILE_VERBOSE) {
        return event_encode_verbose(e, out, out_len);
    }
    if (profile == CSIL_PROFILE_COMPACT) {
        return event_encode_compact(e, out, out_len);
    }
    return CSIL_ERR_MALFORMED;
}

static csil_err event_decode_verbose(csil_arena *a, const csil_cbor_value *v,
                                     csil_event *out) {
    const uint8_t *payload = NULL;
    size_t payload_len = 0;
    const char *event = NULL;
    const csil_cbor_value *p = csil_cbor_map_get(v, "payload");
    csil_err e;
    if ((p == NULL ? (e = CSIL_ERR_MALFORMED)
                   : (e = csil_cbor_untag24(p, &payload, &payload_len))) ||
        (e = env_get_text(v, "event", &event))) {
        return e;
    }
    memset(out, 0, sizeof(*out));
    out->service = env_get_text_opt(v, "service");
    out->event = event;
    out->has_id = env_get_uint_opt(v, "id", &out->id);
    out->payload = payload;
    out->payload_len = payload_len;
    out->_arena = a;
    return CSIL_OK;
}

static csil_err event_decode_compact(csil_arena *a, const csil_cbor_value *v,
                                     csil_event *out) {
    if (v->kind != CSIL_CBOR_ARRAY) {
        return CSIL_ERR_MALFORMED;
    }
    /* 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id. */
    const csil_cbor_value *items = v->as.array.items;
    size_t n = v->as.array.count;
    const csil_cbor_value *service_ord_v, *op_ord_v, *payload_v;
    const csil_cbor_value *id_v = NULL;
    if (n == 3) {
        service_ord_v = &items[0];
        op_ord_v = &items[1];
        payload_v = &items[2];
    } else if (n == 4) {
        service_ord_v = &items[0];
        op_ord_v = &items[1];
        id_v = &items[2];
        payload_v = &items[3];
    } else {
        return CSIL_ERR_MALFORMED;
    }
    memset(out, 0, sizeof(*out));
    if (!csil_cbor_as_u64(service_ord_v, &out->service_ord) ||
        !csil_cbor_as_u64(op_ord_v, &out->op_ord)) {
        return CSIL_ERR_MALFORMED;
    }
    out->has_service_ord = true;
    out->has_op_ord = true;
    csil_err e = csil_cbor_untag24(payload_v, &out->payload, &out->payload_len);
    if (e) {
        return e;
    }
    if (id_v) {
        if (!csil_cbor_as_u64(id_v, &out->id)) {
            return CSIL_ERR_MALFORMED;
        }
        out->has_id = true;
    }
    out->_arena = a;
    return CSIL_OK;
}

csil_err csil_event_decode(const uint8_t *frame, size_t len, csil_profile profile,
                           csil_event *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    if (profile == CSIL_PROFILE_VERBOSE) {
        e = event_decode_verbose(a, v, out);
    } else if (profile == CSIL_PROFILE_COMPACT) {
        e = event_decode_compact(a, v, out);
    } else {
        e = CSIL_ERR_MALFORMED;
    }
    if (e) {
        csil_arena_free(a);
    }
    return e;
}

void csil_event_free(csil_event *e) {
    if (e && e->_arena) {
        csil_arena_free(e->_arena);
        e->_arena = NULL;
    }
}

bool csil_event_equal(const csil_event *a, const csil_event *b) {
    return env_str_eq(a->service, b->service) &&
           a->has_service_ord == b->has_service_ord &&
           (!a->has_service_ord || a->service_ord == b->service_ord) &&
           env_str_eq(a->event, b->event) && a->has_op_ord == b->has_op_ord &&
           (!a->has_op_ord || a->op_ord == b->op_ord) && a->has_id == b->has_id &&
           (!a->has_id || a->id == b->id) &&
           env_bytes_eq(a->payload, a->payload_len, b->payload, b->payload_len);
}

/* ---- hello ---------------------------------------------------------------- */

csil_err csil_hello_encode(const csil_hello *h, uint8_t **out, size_t *out_len) {
    csil_map_builder m;
    csil_map_init(&m);
    csil_map_add_uint_array(&m, "versions", h->versions, h->versions_len);
    csil_map_add_text_array(&m, "profiles", h->profiles, h->profiles_len);
    if (h->service) {
        csil_map_add_text(&m, "service", h->service);
    }
    if (h->auth) {
        csil_map_add_text(&m, "auth", h->auth);
    }
    csil_buf b;
    csil_buf_init(&b);
    csil_err e = csil_map_finish(&m, &b);
    csil_map_dispose(&m);
    return finish_encode(&b, e, out, out_len);
}

csil_err csil_hello_decode(const uint8_t *frame, size_t len, csil_hello *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    const csil_cbor_value *vers = csil_cbor_map_get(v, "versions");
    const csil_cbor_value *profs = csil_cbor_map_get(v, "profiles");
    if (!vers || vers->kind != CSIL_CBOR_ARRAY || !profs ||
        profs->kind != CSIL_CBOR_ARRAY) {
        csil_arena_free(a);
        return CSIL_ERR_MALFORMED;
    }
    memset(out, 0, sizeof(*out));
    size_t vn = vers->as.array.count;
    if (vn) {
        uint64_t *vbuf = csil_arena_alloc(a, vn * sizeof(uint64_t));
        if (!vbuf) {
            csil_arena_free(a);
            return CSIL_ERR_OOM;
        }
        for (size_t i = 0; i < vn; i++) {
            if (!csil_cbor_as_u64(&vers->as.array.items[i], &vbuf[i])) {
                csil_arena_free(a);
                return CSIL_ERR_MALFORMED;
            }
        }
        out->versions = vbuf;
    }
    out->versions_len = vn;
    size_t pn = profs->as.array.count;
    if (pn) {
        const char **pbuf = csil_arena_alloc(a, pn * sizeof(const char *));
        if (!pbuf) {
            csil_arena_free(a);
            return CSIL_ERR_OOM;
        }
        for (size_t i = 0; i < pn; i++) {
            const csil_cbor_value *it = &profs->as.array.items[i];
            if (it->kind != CSIL_CBOR_TEXT) {
                csil_arena_free(a);
                return CSIL_ERR_MALFORMED;
            }
            pbuf[i] = (const char *)it->as.bytes.ptr;
        }
        out->profiles = pbuf;
    }
    out->profiles_len = pn;
    out->service = env_get_text_opt(v, "service");
    out->auth = env_get_text_opt(v, "auth");
    out->_arena = a;
    return CSIL_OK;
}

void csil_hello_free(csil_hello *h) {
    if (h && h->_arena) {
        csil_arena_free(h->_arena);
        h->_arena = NULL;
    }
}

/* ---- hello-ack ------------------------------------------------------------ */

csil_err csil_hello_ack_encode(const csil_hello_ack *a, uint8_t **out,
                               size_t *out_len) {
    csil_map_builder m;
    csil_map_init(&m);
    csil_map_add_uint(&m, "v", a->v);
    csil_map_add_text(&m, "profile", a->profile);
    if (a->session) {
        csil_map_add_text(&m, "session", a->session);
    }
    csil_buf b;
    csil_buf_init(&b);
    csil_err e = csil_map_finish(&m, &b);
    csil_map_dispose(&m);
    return finish_encode(&b, e, out, out_len);
}

csil_err csil_hello_ack_decode(const uint8_t *frame, size_t len,
                               csil_hello_ack *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    uint64_t ver = 0;
    const char *profile = NULL;
    if ((e = env_get_uint(v, "v", &ver)) ||
        (e = env_get_text(v, "profile", &profile))) {
        csil_arena_free(a);
        return e;
    }
    memset(out, 0, sizeof(*out));
    out->v = ver;
    out->profile = profile;
    out->session = env_get_text_opt(v, "session");
    out->_arena = a;
    return CSIL_OK;
}

void csil_hello_ack_free(csil_hello_ack *a) {
    if (a && a->_arena) {
        csil_arena_free(a->_arena);
        a->_arena = NULL;
    }
}

/* ---- heartbeat ------------------------------------------------------------ */

csil_err csil_heartbeat_encode(const csil_heartbeat *h, uint8_t **out,
                               size_t *out_len) {
    csil_map_builder m;
    csil_map_init(&m);
    csil_map_add_uint(&m, "nonce", h->nonce);
    if (h->has_at) {
        csil_map_add_uint(&m, "at", h->at);
    }
    csil_buf b;
    csil_buf_init(&b);
    csil_err e = csil_map_finish(&m, &b);
    csil_map_dispose(&m);
    return finish_encode(&b, e, out, out_len);
}

csil_err csil_heartbeat_decode(const uint8_t *frame, size_t len,
                               csil_heartbeat *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    uint64_t nonce = 0;
    if ((e = env_get_uint(v, "nonce", &nonce))) {
        csil_arena_free(a);
        return e;
    }
    memset(out, 0, sizeof(*out));
    out->nonce = nonce;
    out->has_at = env_get_uint_opt(v, "at", &out->at);
    out->_arena = a;
    return CSIL_OK;
}

void csil_heartbeat_free(csil_heartbeat *h) {
    if (h && h->_arena) {
        csil_arena_free(h->_arena);
        h->_arena = NULL;
    }
}

/* ---- close ---------------------------------------------------------------- */

csil_err csil_close_encode(const csil_close *c, uint8_t **out, size_t *out_len) {
    csil_map_builder m;
    csil_map_init(&m);
    csil_map_add_int(&m, "status", csil_status_code(c->status));
    if (c->reason) {
        csil_map_add_text(&m, "reason", c->reason);
    }
    csil_buf b;
    csil_buf_init(&b);
    csil_err e = csil_map_finish(&m, &b);
    csil_map_dispose(&m);
    return finish_encode(&b, e, out, out_len);
}

csil_err csil_close_decode(const uint8_t *frame, size_t len, csil_close *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    int64_t status = 0;
    if ((e = env_get_int(v, "status", &status))) {
        csil_arena_free(a);
        return e;
    }
    memset(out, 0, sizeof(*out));
    out->status = csil_status_from_code(status);
    out->reason = env_get_text_opt(v, "reason");
    out->_arena = a;
    return CSIL_OK;
}

void csil_close_free(csil_close *c) {
    if (c && c->_arena) {
        csil_arena_free(c->_arena);
        c->_arena = NULL;
    }
}
