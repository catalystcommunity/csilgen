#include "csil/csil_rpc.h"

#include "envelope_internal.h"

#include <stdlib.h>
#include <string.h>

/* Finalize an encode: hand the buffer's data off to the caller, or release it on
 * the sticky error. The buffer's data pointer is plain malloc'd memory, so the
 * caller frees it with csil_free. */
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

/* ---- request -------------------------------------------------------------- */

void csil_rpc_request_init(csil_rpc_request *r, const char *service, const char *op,
                           const uint8_t *payload, size_t payload_len) {
    memset(r, 0, sizeof(*r));
    r->service = service;
    r->op = op;
    r->payload = payload;
    r->payload_len = payload_len;
}

csil_err csil_rpc_request_encode(const csil_rpc_request *r, uint8_t **out,
                                 size_t *out_len) {
    csil_map_builder m;
    csil_map_init(&m);
    csil_map_add_uint(&m, "v", CSIL_VERSION);
    csil_map_add_text(&m, "service", r->service);
    csil_map_add_text(&m, "op", r->op);
    csil_map_add_payload(&m, "payload", r->payload, r->payload_len);
    if (r->has_id) {
        csil_map_add_uint(&m, "id", r->id);
    }
    if (r->auth) {
        csil_map_add_text(&m, "auth", r->auth);
    }
    csil_buf b;
    csil_buf_init(&b);
    csil_err e = csil_map_finish(&m, &b);
    csil_map_dispose(&m);
    return finish_encode(&b, e, out, out_len);
}

csil_err csil_rpc_request_decode(const uint8_t *frame, size_t len,
                                 csil_rpc_request *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    uint64_t ver = 0;
    const uint8_t *payload = NULL;
    size_t payload_len = 0;
    const char *service = NULL;
    const char *op = NULL;
    const csil_cbor_value *p = csil_cbor_map_get(v, "payload");
    if ((e = env_get_uint(v, "v", &ver)) || (e = env_check_version(ver)) ||
        (p == NULL ? (e = CSIL_ERR_MALFORMED) : (e = csil_cbor_untag24(p, &payload, &payload_len))) ||
        (e = env_get_text(v, "service", &service)) ||
        (e = env_get_text(v, "op", &op))) {
        csil_arena_free(a);
        return e;
    }
    memset(out, 0, sizeof(*out));
    out->service = service;
    out->op = op;
    out->payload = payload;
    out->payload_len = payload_len;
    out->has_id = env_get_uint_opt(v, "id", &out->id);
    out->auth = env_get_text_opt(v, "auth");
    out->_arena = a;
    return CSIL_OK;
}

void csil_rpc_request_free(csil_rpc_request *r) {
    if (r && r->_arena) {
        csil_arena_free(r->_arena);
        r->_arena = NULL;
    }
}

bool csil_rpc_request_equal(const csil_rpc_request *a, const csil_rpc_request *b) {
    return env_str_eq(a->service, b->service) && env_str_eq(a->op, b->op) &&
           a->has_id == b->has_id && (!a->has_id || a->id == b->id) &&
           env_str_eq(a->auth, b->auth) &&
           env_bytes_eq(a->payload, a->payload_len, b->payload, b->payload_len);
}

/* ---- response ------------------------------------------------------------- */

void csil_rpc_response_init_ok(csil_rpc_response *r, const char *variant,
                               const uint8_t *payload, size_t payload_len) {
    memset(r, 0, sizeof(*r));
    r->status = csil_status_ok();
    r->variant = variant;
    r->payload = payload;
    r->payload_len = payload_len;
}

void csil_rpc_response_init_transport_error(csil_rpc_response *r, csil_status status,
                                            const char *message) {
    memset(r, 0, sizeof(*r));
    r->status = status;
    r->error = message;
    r->payload = NULL;
    r->payload_len = 0;
}

csil_err csil_rpc_response_encode(const csil_rpc_response *r, uint8_t **out,
                                  size_t *out_len) {
    csil_map_builder m;
    csil_map_init(&m);
    csil_map_add_uint(&m, "v", CSIL_VERSION);
    csil_map_add_int(&m, "status", csil_status_code(r->status));
    csil_map_add_payload(&m, "payload", r->payload, r->payload_len);
    if (r->has_id) {
        csil_map_add_uint(&m, "id", r->id);
    }
    if (r->variant) {
        csil_map_add_text(&m, "variant", r->variant);
    }
    if (r->error) {
        csil_map_add_text(&m, "error", r->error);
    }
    csil_buf b;
    csil_buf_init(&b);
    csil_err e = csil_map_finish(&m, &b);
    csil_map_dispose(&m);
    return finish_encode(&b, e, out, out_len);
}

csil_err csil_rpc_response_decode(const uint8_t *frame, size_t len,
                                  csil_rpc_response *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    uint64_t ver = 0;
    int64_t status = 0;
    if ((e = env_get_uint(v, "v", &ver)) || (e = env_check_version(ver)) ||
        (e = env_get_int(v, "status", &status))) {
        csil_arena_free(a);
        return e;
    }
    /* payload is present but may be an empty byte string on error. */
    const uint8_t *payload = NULL;
    size_t payload_len = 0;
    const csil_cbor_value *p = csil_cbor_map_get(v, "payload");
    if (p) {
        if ((e = csil_cbor_untag24(p, &payload, &payload_len))) {
            csil_arena_free(a);
            return e;
        }
    }
    memset(out, 0, sizeof(*out));
    out->has_id = env_get_uint_opt(v, "id", &out->id);
    out->status = csil_status_from_code(status);
    out->variant = env_get_text_opt(v, "variant");
    out->error = env_get_text_opt(v, "error");
    out->payload = payload;
    out->payload_len = payload_len;
    out->_arena = a;
    return CSIL_OK;
}

void csil_rpc_response_free(csil_rpc_response *r) {
    if (r && r->_arena) {
        csil_arena_free(r->_arena);
        r->_arena = NULL;
    }
}

bool csil_rpc_response_equal(const csil_rpc_response *a, const csil_rpc_response *b) {
    return a->has_id == b->has_id && (!a->has_id || a->id == b->id) &&
           a->status.code == b->status.code && env_str_eq(a->variant, b->variant) &&
           env_str_eq(a->error, b->error) &&
           env_bytes_eq(a->payload, a->payload_len, b->payload, b->payload_len);
}

bool csil_rpc_response_is_transport_error(const csil_rpc_response *r) {
    return !csil_status_is_ok(r->status);
}

/* ---- push ----------------------------------------------------------------- */

void csil_rpc_push_init(csil_rpc_push *p, const char *service, const char *event,
                        const uint8_t *payload, size_t payload_len) {
    memset(p, 0, sizeof(*p));
    p->service = service;
    p->event = event;
    p->payload = payload;
    p->payload_len = payload_len;
}

csil_err csil_rpc_push_encode(const csil_rpc_push *p, uint8_t **out,
                              size_t *out_len) {
    csil_map_builder m;
    csil_map_init(&m);
    csil_map_add_uint(&m, "v", CSIL_VERSION);
    csil_map_add_text(&m, "service", p->service);
    csil_map_add_text(&m, "event", p->event);
    csil_map_add_payload(&m, "payload", p->payload, p->payload_len);
    csil_buf b;
    csil_buf_init(&b);
    csil_err e = csil_map_finish(&m, &b);
    csil_map_dispose(&m);
    return finish_encode(&b, e, out, out_len);
}

csil_err csil_rpc_push_decode(const uint8_t *frame, size_t len,
                              csil_rpc_push *out) {
    csil_arena *a = NULL;
    csil_cbor_value *v = NULL;
    csil_err e = csil_cbor_decode_envelope(frame, len, &a, &v);
    if (e) {
        return e;
    }
    uint64_t ver = 0;
    const uint8_t *payload = NULL;
    size_t payload_len = 0;
    const char *service = NULL;
    const char *event = NULL;
    const csil_cbor_value *p = csil_cbor_map_get(v, "payload");
    if ((e = env_get_uint(v, "v", &ver)) || (e = env_check_version(ver)) ||
        (p == NULL ? (e = CSIL_ERR_MALFORMED) : (e = csil_cbor_untag24(p, &payload, &payload_len))) ||
        (e = env_get_text(v, "service", &service)) ||
        (e = env_get_text(v, "event", &event))) {
        csil_arena_free(a);
        return e;
    }
    memset(out, 0, sizeof(*out));
    out->service = service;
    out->event = event;
    out->payload = payload;
    out->payload_len = payload_len;
    out->_arena = a;
    return CSIL_OK;
}

void csil_rpc_push_free(csil_rpc_push *p) {
    if (p && p->_arena) {
        csil_arena_free(p->_arena);
        p->_arena = NULL;
    }
}

bool csil_rpc_push_equal(const csil_rpc_push *a, const csil_rpc_push *b) {
    return env_str_eq(a->service, b->service) && env_str_eq(a->event, b->event) &&
           env_bytes_eq(a->payload, a->payload_len, b->payload, b->payload_len);
}

/* ---- client --------------------------------------------------------------- */

struct csil_rpc_client {
    csil_frame_carrier carrier;
    uint64_t next_id;
    bool multiplexed;
};

csil_rpc_client *csil_rpc_client_new(const csil_frame_carrier *carrier,
                                     bool multiplexed) {
    csil_rpc_client *c = calloc(1, sizeof(*c));
    if (!c) {
        return NULL;
    }
    c->carrier = *carrier;
    c->next_id = 1;
    c->multiplexed = multiplexed;
    return c;
}

void csil_rpc_client_free(csil_rpc_client *c) { free(c); }

csil_err csil_rpc_client_call(csil_rpc_client *c, const char *service,
                              const char *op, const uint8_t *payload,
                              size_t payload_len, const char *auth,
                              csil_rpc_response *out) {
    csil_rpc_request req;
    csil_rpc_request_init(&req, service, op, payload, payload_len);
    req.auth = auth;
    if (c->multiplexed) {
        req.id = c->next_id++;
        req.has_id = true;
    }
    uint8_t *frame = NULL;
    size_t frame_len = 0;
    csil_err e = csil_rpc_request_encode(&req, &frame, &frame_len);
    if (e) {
        return e;
    }
    e = c->carrier.send_frame(c->carrier.userdata, frame, frame_len);
    csil_free(frame);
    if (e) {
        return e;
    }
    uint8_t *in = NULL;
    size_t in_len = 0;
    e = c->carrier.recv_frame(c->carrier.userdata, &in, &in_len);
    if (e) {
        return e;
    }
    if (in == NULL) {
        return CSIL_ERR_CARRIER; /* connection closed before a response */
    }
    e = csil_rpc_response_decode(in, in_len, out);
    csil_free(in);
    return e;
}

/* ---- server --------------------------------------------------------------- */

void csil_rpc_outcome_reply(csil_rpc_outcome *o, const char *variant,
                            const uint8_t *payload, size_t payload_len) {
    memset(o, 0, sizeof(*o));
    o->is_reply = true;
    o->variant = variant;
    o->payload = payload;
    o->payload_len = payload_len;
}

void csil_rpc_outcome_transport(csil_rpc_outcome *o, csil_status status,
                                const char *message) {
    memset(o, 0, sizeof(*o));
    o->is_reply = false;
    o->status = status;
    o->message = message;
}

struct csil_rpc_server {
    csil_frame_carrier carrier;
};

csil_rpc_server *csil_rpc_server_new(const csil_frame_carrier *carrier) {
    csil_rpc_server *s = calloc(1, sizeof(*s));
    if (!s) {
        return NULL;
    }
    s->carrier = *carrier;
    return s;
}

void csil_rpc_server_free(csil_rpc_server *s) { free(s); }

csil_err csil_rpc_server_serve_one(csil_rpc_server *s, csil_rpc_handler handler,
                                   void *ctx, bool *served) {
    *served = false;
    uint8_t *frame = NULL;
    size_t frame_len = 0;
    csil_err e = s->carrier.recv_frame(s->carrier.userdata, &frame, &frame_len);
    if (e) {
        return e;
    }
    if (frame == NULL) {
        return CSIL_OK; /* clean end of stream */
    }

    csil_rpc_response resp;
    csil_rpc_request req;
    csil_err derr = csil_rpc_request_decode(frame, frame_len, &req);
    csil_free(frame);
    /* The outcome's reply payload may borrow from the request's arena, so the
     * request stays alive until after the response is encoded. */
    if (derr == CSIL_OK) {
        csil_rpc_outcome outcome;
        memset(&outcome, 0, sizeof(outcome));
        handler(ctx, &req, &outcome);
        if (outcome.is_reply) {
            csil_rpc_response_init_ok(&resp, outcome.variant, outcome.payload,
                                      outcome.payload_len);
        } else {
            csil_rpc_response_init_transport_error(&resp, outcome.status,
                                                   outcome.message);
        }
        resp.has_id = req.has_id;
        resp.id = req.id;
    } else {
        csil_rpc_response_init_transport_error(
            &resp, csil_status_from_code(CSIL_STATUS_MALFORMED_ENVELOPE_CODE),
            csil_err_str(derr));
    }

    uint8_t *out = NULL;
    size_t out_len = 0;
    e = csil_rpc_response_encode(&resp, &out, &out_len);
    if (derr == CSIL_OK) {
        csil_rpc_request_free(&req);
    }
    if (e) {
        return e;
    }
    e = s->carrier.send_frame(s->carrier.userdata, out, out_len);
    csil_free(out);
    if (e) {
        return e;
    }
    *served = true;
    return CSIL_OK;
}
