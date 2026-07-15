/* C interop harness.
 *
 * Mirrors the Rust reference harness (tests/interop/harness/rust/src/main.rs):
 * runs as a server (binds a loopback TCP/UDP port, serves one transport) or a
 * client (connects, runs the case battery for one transport, prints one JSON
 * line). Stream transports (rpc, events) ride loopback TCP fed into the lib's
 * length-prefix stream carrier; datagrams ride loopback UDP with the lib's
 * datagram envelope codec.
 */

/* nanosleep, struct timeval (SO_RCVTIMEO), and the BSD socket API are POSIX,
 * which strict -std=c11 hides without this feature-test macro. */
#define _POSIX_C_SOURCE 200809L

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <time.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <netinet/in.h>
#include <arpa/inet.h>

#include "csil/csil.h"

#include "gen/types.gen.h"
#include "gen/codec.gen.h"
#include "gen/validation.gen.h"

#define SERVICE "Interop"

/* --------------------------------------------------------------------------
 * Fixed language-neutral test vectors (see tests/interop/README.md).
 * Pointer/array members reference function-static storage so a by-value return
 * stays valid for the lifetime of the process.
 * ------------------------------------------------------------------------ */

static Scalars scalars_ok(void) {
    static uint8_t raw[] = {0x01, 0x02, 0xf0, 0xff};
    Scalars s;
    s.i = -42;
    s.u = 42;
    s.n = -7;
    s.f = 3.5;
    s.t = "h\xC3\xA9llo \xE4\xB8\x96\xE7\x95\x8C"; /* "héllo 世界" */
    s.raw.data = raw;
    s.raw.len = sizeof(raw);
    s.flag = true;
    s.when.rfc3339 = "2026-06-29T12:34:56Z";
    s.when.epoch_seconds = 0;
    s.amount.exponent = -2; /* 123.45 = 12345 * 10^-2 */
    s.amount.mantissa = 12345;
    /* mixed-union coverage: a value equal to a literal arm must wire as
     * [1,"pending"]; a value with no literal match must wire as [0,...]. */
    s.status_literal.tag = MIXED_STATUS_CHOICE1;
    s.status_literal.u.choice1 = "pending";
    s.status_free.tag = MIXED_STATUS_TEXT;
    s.status_free.u.text = "unlisted";
    /* inline mixed choice (hoisted; literal arm match). */
    s.note.tag = SCALARS_NOTE_CHOICE1;
    s.note.u.choice1 = "info";
    /* inline all-literal choice (hoisted to a plain enum). */
    s.size = SCALARS_SIZE_MEDIUM;
    /* named enum with a trailing `.default`-constrained last arm. */
    s.level = LEVEL_HIGH;
    /* named enum assembled via base rule + `/=` extension. */
    s.season = SEASON_AUTUMN;
    /* named all-literal enum with mixed literal kinds (text + int arms);
     * renders as a tagged struct (no single C type spans both kinds). */
    s.ship_text.tag = SHIP_MODE_GROUND;
    s.ship_text.u.ground = "ground";
    s.ship_int.tag = SHIP_MODE_2;
    s.ship_int.u.v2 = 2;
    return s;
}

static Collections collections_ok(void) {
    static char *names[] = {"a", "b"};
    static int64_t at_least_one[] = {1, 2, 3};
    static int64_t bounded[] = {10, 20};
    static int64_t exact3[] = {7, 8, 9};
    static char *scores_keys[] = {"x", "y"};
    static int64_t scores_values[] = {1, 2};
    static int64_t triple_f1 = 9;
    static bool triple_f2 = true;
    static const csilc_value extra_v = {CSILC_TEXT, {.bytes = {(const uint8_t *)"v", 1}}};
    static char *extra_keys[] = {"k"};
    static const csilc_value *extra_values[] = {&extra_v};

    Collections c;
    c.names = names;
    c.names_count = 2;
    c.at_least_one = at_least_one;
    c.at_least_one_count = 3;
    c.bounded = bounded;
    c.bounded_count = 2;
    c.exact3 = exact3;
    c.exact3_count = 3;
    c.scores_keys = scores_keys;
    c.scores_values = scores_values;
    c.scores_count = 2;
    c.color = COLOR_GREEN;
    c.tone = COLOR_BLUE;
    c.prio = PRIORITY_2;
    c.who.tag = ID_OR_NAME_UINT;
    c.who.u.uint = 4242;
    c.pair.f0 = "p";
    c.pair.f1 = 5;
    c.triple.f0 = "t";
    c.triple.f1 = &triple_f1;
    c.triple.f2 = &triple_f2;
    c.extra_keys = extra_keys;
    c.extra_values = extra_values;
    c.extra_count = 1;
    return c;
}

static Constrained constrained_ok(void) {
    static char *tags[] = {"one", "two"};
    Constrained c;
    c.code = "PRD-AB12CD";
    c.qty = 10;
    c.rate.exponent = -2; /* 0.25 */
    c.rate.mantissa = 25;
    c.password = "hunter2hunter2";
    c.tags = tags;
    c.tags_count = 2;
    return c;
}

static Constrained constrained_bad(void) {
    Constrained c;
    c.code = "bad";
    c.qty = 0;
    c.rate.exponent = -1; /* 9.9 */
    c.rate.mantissa = 99;
    c.password = "x";
    c.tags = NULL;
    c.tags_count = 0;
    return c;
}

static Nested nested_ok(void) {
    static Scalars many[1];
    static Constrained maybe_store;
    many[0] = scalars_ok();
    maybe_store = constrained_ok();
    Nested n;
    n.inner = scalars_ok();
    n.maybe = &maybe_store;
    n.many = many;
    n.many_count = 1;
    return n;
}

/* --------------------------------------------------------------------------
 * Structural equality (decoded value vs the expected vector).
 * ------------------------------------------------------------------------ */

static bool streq(const char *a, const char *b) {
    if (!a || !b) return a == b;
    return strcmp(a, b) == 0;
}

static bool mixed_status_eq(const MixedStatus *a, const MixedStatus *b) {
    if (a->tag != b->tag) return false;
    switch (a->tag) {
        case MIXED_STATUS_TEXT: return streq(a->u.text, b->u.text);
        case MIXED_STATUS_CHOICE1: return streq(a->u.choice1, b->u.choice1);
        case MIXED_STATUS_CHOICE2: return streq(a->u.choice2, b->u.choice2);
        case MIXED_STATUS_CHOICE3: return streq(a->u.choice3, b->u.choice3);
    }
    return false;
}

static bool scalars_note_eq(const Scalars_note *a, const Scalars_note *b) {
    if (a->tag != b->tag) return false;
    switch (a->tag) {
        case SCALARS_NOTE_TEXT: return streq(a->u.text, b->u.text);
        case SCALARS_NOTE_CHOICE1: return streq(a->u.choice1, b->u.choice1);
        case SCALARS_NOTE_CHOICE2: return streq(a->u.choice2, b->u.choice2);
    }
    return false;
}

static bool ship_mode_eq(const ShipMode *a, const ShipMode *b) {
    if (a->tag != b->tag) return false;
    switch (a->tag) {
        case SHIP_MODE_GROUND: return streq(a->u.ground, b->u.ground);
        case SHIP_MODE_2: return a->u.v2 == b->u.v2;
        case SHIP_MODE_AIR: return streq(a->u.air, b->u.air);
    }
    return false;
}

static bool scalars_eq(const Scalars *a, const Scalars *b) {
    if (a->i != b->i || a->u != b->u || a->n != b->n) return false;
    if (a->f != b->f) return false;
    if (!streq(a->t, b->t)) return false;
    if (a->raw.len != b->raw.len || memcmp(a->raw.data, b->raw.data, a->raw.len) != 0)
        return false;
    if (a->flag != b->flag) return false;
    if (!streq(a->when.rfc3339, b->when.rfc3339)) return false;
    if (a->amount.exponent != b->amount.exponent || a->amount.mantissa != b->amount.mantissa)
        return false;
    if (!mixed_status_eq(&a->status_literal, &b->status_literal)) return false;
    if (!mixed_status_eq(&a->status_free, &b->status_free)) return false;
    if (!scalars_note_eq(&a->note, &b->note)) return false;
    if (a->size != b->size) return false;
    if (a->level != b->level) return false;
    if (a->season != b->season) return false;
    if (!ship_mode_eq(&a->ship_text, &b->ship_text)) return false;
    if (!ship_mode_eq(&a->ship_int, &b->ship_int)) return false;
    return true;
}

static bool constrained_eq(const Constrained *a, const Constrained *b) {
    if (!streq(a->code, b->code) || a->qty != b->qty) return false;
    if (a->rate.exponent != b->rate.exponent || a->rate.mantissa != b->rate.mantissa)
        return false;
    if (!streq(a->password, b->password)) return false;
    if (a->tags_count != b->tags_count) return false;
    for (size_t i = 0; i < a->tags_count; i++)
        if (!streq(a->tags[i], b->tags[i])) return false;
    return true;
}

static bool i64_arr_eq(const int64_t *a, size_t an, const int64_t *b, size_t bn) {
    if (an != bn) return false;
    for (size_t i = 0; i < an; i++)
        if (a[i] != b[i]) return false;
    return true;
}

static bool any_is_text(const csilc_value *v, const char *txt) {
    size_t n = strlen(txt);
    return v && v->kind == CSILC_TEXT && v->as.bytes.len == n &&
           memcmp(v->as.bytes.ptr, txt, n) == 0;
}

static bool collections_eq(const Collections *a, const Collections *b) {
    if (a->names_count != b->names_count) return false;
    for (size_t i = 0; i < a->names_count; i++)
        if (!streq(a->names[i], b->names[i])) return false;
    if (!i64_arr_eq(a->at_least_one, a->at_least_one_count, b->at_least_one, b->at_least_one_count))
        return false;
    if (!i64_arr_eq(a->bounded, a->bounded_count, b->bounded, b->bounded_count)) return false;
    if (!i64_arr_eq(a->exact3, a->exact3_count, b->exact3, b->exact3_count)) return false;
    /* Maps are unordered (the Rust reference stores scores/extra in a HashMap, so
     * the echoed entry order is arbitrary); compare by key lookup, not positionally. */
    if (a->scores_count != b->scores_count) return false;
    for (size_t i = 0; i < a->scores_count; i++) {
        bool found = false;
        for (size_t j = 0; j < b->scores_count; j++) {
            if (streq(a->scores_keys[i], b->scores_keys[j])) {
                if (a->scores_values[i] != b->scores_values[j]) return false;
                found = true;
                break;
            }
        }
        if (!found) return false;
    }
    if (a->color != b->color || a->tone != b->tone || a->prio != b->prio) return false;
    if (a->who.tag != b->who.tag) return false;
    if (a->who.tag == ID_OR_NAME_UINT && a->who.u.uint != b->who.u.uint) return false;
    if (a->who.tag == ID_OR_NAME_TEXT && !streq(a->who.u.text, b->who.u.text)) return false;
    if (!streq(a->pair.f0, b->pair.f0) || a->pair.f1 != b->pair.f1) return false;
    if (!streq(a->triple.f0, b->triple.f0)) return false;
    if ((a->triple.f1 == NULL) != (b->triple.f1 == NULL)) return false;
    if (a->triple.f1 && *a->triple.f1 != *b->triple.f1) return false;
    if ((a->triple.f2 == NULL) != (b->triple.f2 == NULL)) return false;
    if (a->triple.f2 && *a->triple.f2 != *b->triple.f2) return false;
    if (a->extra_count != b->extra_count) return false;
    for (size_t i = 0; i < a->extra_count; i++) {
        bool found = false;
        for (size_t j = 0; j < b->extra_count; j++) {
            if (streq(a->extra_keys[i], b->extra_keys[j])) {
                /* The expected vector carries text "v"; assert both sides match. */
                if (!any_is_text(a->extra_values[i], "v") ||
                    !any_is_text(b->extra_values[j], "v"))
                    return false;
                found = true;
                break;
            }
        }
        if (!found) return false;
    }
    return true;
}

static bool nested_eq(const Nested *a, const Nested *b) {
    if (!scalars_eq(&a->inner, &b->inner)) return false;
    if ((a->maybe == NULL) != (b->maybe == NULL)) return false;
    if (a->maybe && !constrained_eq(a->maybe, b->maybe)) return false;
    if (a->many_count != b->many_count) return false;
    for (size_t i = 0; i < a->many_count; i++)
        if (!scalars_eq(&a->many[i], &b->many[i])) return false;
    return true;
}

/* --------------------------------------------------------------------------
 * Result accumulation + JSON output (one line, no dependency).
 * ------------------------------------------------------------------------ */

typedef struct Case {
    const char *name;
    bool ok;
    char detail[256];
} Case;

typedef struct Cases {
    const char *transport;
    Case items[16];
    size_t count;
} Cases;

static void cases_init(Cases *c, const char *transport) {
    c->transport = transport;
    c->count = 0;
}

static void cases_add(Cases *c, const char *name, bool ok, const char *detail) {
    if (c->count >= sizeof(c->items) / sizeof(c->items[0])) return;
    Case *it = &c->items[c->count++];
    it->name = name;
    it->ok = ok;
    snprintf(it->detail, sizeof(it->detail), "%s", detail ? detail : "");
}

static void json_escape(const char *s, char *out, size_t cap) {
    size_t o = 0;
    for (size_t i = 0; s && s[i] && o + 2 < cap; i++) {
        char ch = s[i];
        if (ch == '"' || ch == '\\') {
            out[o++] = '\\';
            out[o++] = ch;
        } else if ((unsigned char)ch < 0x20) {
            o += (size_t)snprintf(out + o, cap - o, "\\u%04x", ch);
        } else {
            out[o++] = ch;
        }
    }
    out[o < cap ? o : cap - 1] = 0;
}

static void cases_emit(const Cases *c) {
    char buf[512];
    printf("{\"lang\":\"c\",\"transport\":\"%s\",\"cases\":[", c->transport);
    for (size_t i = 0; i < c->count; i++) {
        if (i) printf(",");
        json_escape(c->items[i].detail, buf, sizeof(buf));
        printf("{\"name\":\"%s\",\"ok\":%s,\"detail\":\"%s\"}", c->items[i].name,
               c->items[i].ok ? "true" : "false", buf);
    }
    printf("]}\n");
    fflush(stdout);
}

/* --------------------------------------------------------------------------
 * Socket <-> csil_stream seam for the length-prefix stream carrier.
 * ------------------------------------------------------------------------ */

static long sock_read(void *ud, uint8_t *buf, size_t len) {
    int fd = *(int *)ud;
    ssize_t n = read(fd, buf, len);
    if (n < 0) return -1;
    return (long)n;
}

static int sock_write(void *ud, const uint8_t *buf, size_t len) {
    int fd = *(int *)ud;
    size_t off = 0;
    while (off < len) {
        ssize_t n = write(fd, buf + off, len - off);
        if (n <= 0) return -1;
        off += (size_t)n;
    }
    return 0;
}

/* --------------------------------------------------------------------------
 * RPC
 * ------------------------------------------------------------------------ */

typedef struct RpcCtx {
    uint8_t *pending; /* the previous reply's payload; freed before the next one */
} RpcCtx;

static void rpc_handler(void *vctx, const csil_rpc_request *req, csil_rpc_outcome *out) {
    RpcCtx *ctx = (RpcCtx *)vctx;
    /* The prior reply's bytes were already encoded+sent by serve_one; reclaim them. */
    free(ctx->pending);
    ctx->pending = NULL;

    uint8_t *pb = NULL;
    size_t pn = 0;
    CsilCodecArena *own = NULL;

    if (strcmp(req->op, "echo-scalars") == 0) {
        Scalars s;
        if (csil_decode_Scalars(req->payload, req->payload_len, &s, &own)) {
            csil_rpc_outcome_transport(
                out, csil_status_from_code(CSIL_STATUS_MALFORMED_ENVELOPE_CODE), "decode");
            return;
        }
        int rc = csil_encode_Scalars(&s, &pb, &pn);
        csil_codec_arena_free(own);
        if (rc) {
            csil_rpc_outcome_transport(out, csil_status_from_code(CSIL_STATUS_INTERNAL_CODE),
                                       "encode");
            return;
        }
        ctx->pending = pb;
        csil_rpc_outcome_reply(out, "Scalars", pb, pn);
    } else if (strcmp(req->op, "echo-collections") == 0) {
        Collections v;
        if (csil_decode_Collections(req->payload, req->payload_len, &v, &own)) {
            csil_rpc_outcome_transport(
                out, csil_status_from_code(CSIL_STATUS_MALFORMED_ENVELOPE_CODE), "decode");
            return;
        }
        int rc = csil_encode_Collections(&v, &pb, &pn);
        csil_codec_arena_free(own);
        if (rc) {
            csil_rpc_outcome_transport(out, csil_status_from_code(CSIL_STATUS_INTERNAL_CODE),
                                       "encode");
            return;
        }
        ctx->pending = pb;
        csil_rpc_outcome_reply(out, "Collections", pb, pn);
    } else if (strcmp(req->op, "echo-nested") == 0) {
        Nested v;
        if (csil_decode_Nested(req->payload, req->payload_len, &v, &own)) {
            csil_rpc_outcome_transport(
                out, csil_status_from_code(CSIL_STATUS_MALFORMED_ENVELOPE_CODE), "decode");
            return;
        }
        EchoNestedResult r;
        r.ok = true;
        r.echo = v;
        int rc = csil_encode_EchoNestedResult(&r, &pb, &pn);
        csil_codec_arena_free(own);
        if (rc) {
            csil_rpc_outcome_transport(out, csil_status_from_code(CSIL_STATUS_INTERNAL_CODE),
                                       "encode");
            return;
        }
        ctx->pending = pb;
        csil_rpc_outcome_reply(out, "EchoNestedResult", pb, pn);
    } else if (strcmp(req->op, "validate-constrained") == 0) {
        Constrained v;
        if (csil_decode_Constrained(req->payload, req->payload_len, &v, &own)) {
            csil_rpc_outcome_transport(
                out, csil_status_from_code(CSIL_STATUS_MALFORMED_ENVELOPE_CODE), "decode");
            return;
        }
        if (Constrained_validate(&v)) {
            int rc = csil_encode_Constrained(&v, &pb, &pn);
            csil_codec_arena_free(own);
            if (rc) {
                csil_rpc_outcome_transport(out, csil_status_from_code(CSIL_STATUS_INTERNAL_CODE),
                                           "encode");
                return;
            }
            ctx->pending = pb;
            csil_rpc_outcome_reply(out, "Constrained", pb, pn);
        } else {
            /* The typed error arm rides as a status-0 `ServiceError` variant. */
            csil_codec_arena_free(own);
            ServiceError e;
            e.code = 422;
            e.message = "validation failed";
            if (csil_encode_ServiceError(&e, &pb, &pn)) {
                csil_rpc_outcome_transport(out, csil_status_from_code(CSIL_STATUS_INTERNAL_CODE),
                                           "encode");
                return;
            }
            ctx->pending = pb;
            csil_rpc_outcome_reply(out, "ServiceError", pb, pn);
        }
    } else {
        csil_rpc_outcome_transport(
            out, csil_status_from_code(CSIL_STATUS_UNKNOWN_SERVICE_OR_OP_CODE), "unknown op");
    }
}

static void rpc_serve_conn(int fd) {
    csil_stream st = {sock_read, sock_write, &fd};
    csil_frame_carrier car = csil_stream_carrier(&st, 0);
    csil_rpc_server *srv = csil_rpc_server_new(&car);
    RpcCtx ctx = {NULL};
    for (;;) {
        bool served = false;
        if (csil_rpc_server_serve_one(srv, rpc_handler, &ctx, &served) != CSIL_OK || !served)
            break;
    }
    free(ctx.pending);
    csil_rpc_server_free(srv);
    csil_stream_carrier_dispose(&car);
}

/* One typed RPC call: encode req, call, classify the reply (ok / ServiceError /
 * transport error). Returns 0 with *resp populated on a successful (any-status)
 * round-trip; the caller inspects *resp. */
static int rpc_call(csil_rpc_client *cl, const char *op, const uint8_t *req, size_t reqn,
                    csil_rpc_response *resp) {
    return csil_rpc_client_call(cl, SERVICE, op, req, reqn, NULL, resp);
}

static void rpc_client_run(int fd, Cases *cases) {
    csil_stream st = {sock_read, sock_write, &fd};
    csil_frame_carrier car = csil_stream_carrier(&st, 0);
    csil_rpc_client *cl = csil_rpc_client_new(&car, false);

    /* echo-scalars */
    {
        Scalars want = scalars_ok();
        uint8_t *rb = NULL;
        size_t rn = 0;
        csil_rpc_response resp;
        if (csil_encode_Scalars(&want, &rb, &rn)) {
            cases_add(cases, "echo-scalars/success", false, "encode");
        } else if (rpc_call(cl, "echo-scalars", rb, rn, &resp)) {
            free(rb);
            cases_add(cases, "echo-scalars/success", false, "carrier");
        } else {
            free(rb);
            Scalars got;
            CsilCodecArena *own = NULL;
            if (!csil_status_is_ok(resp.status)) {
                cases_add(cases, "echo-scalars/success", false, "transport error");
            } else if (csil_decode_Scalars(resp.payload, resp.payload_len, &got, &own)) {
                cases_add(cases, "echo-scalars/success", false, "decode");
            } else {
                cases_add(cases, "echo-scalars/success", scalars_eq(&got, &want), "mismatch");
                csil_codec_arena_free(own);
            }
            csil_rpc_response_free(&resp);
        }
    }

    /* echo-collections */
    {
        Collections want = collections_ok();
        uint8_t *rb = NULL;
        size_t rn = 0;
        csil_rpc_response resp;
        if (csil_encode_Collections(&want, &rb, &rn)) {
            cases_add(cases, "echo-collections/success", false, "encode");
        } else if (rpc_call(cl, "echo-collections", rb, rn, &resp)) {
            free(rb);
            cases_add(cases, "echo-collections/success", false, "carrier");
        } else {
            free(rb);
            Collections got;
            CsilCodecArena *own = NULL;
            if (!csil_status_is_ok(resp.status)) {
                cases_add(cases, "echo-collections/success", false, "transport error");
            } else if (csil_decode_Collections(resp.payload, resp.payload_len, &got, &own)) {
                cases_add(cases, "echo-collections/success", false, "decode");
            } else {
                cases_add(cases, "echo-collections/success", collections_eq(&got, &want),
                          "mismatch");
                csil_codec_arena_free(own);
            }
            csil_rpc_response_free(&resp);
        }
    }

    /* echo-nested */
    {
        Nested want = nested_ok();
        uint8_t *rb = NULL;
        size_t rn = 0;
        csil_rpc_response resp;
        if (csil_encode_Nested(&want, &rb, &rn)) {
            cases_add(cases, "echo-nested/success", false, "encode");
        } else if (rpc_call(cl, "echo-nested", rb, rn, &resp)) {
            free(rb);
            cases_add(cases, "echo-nested/success", false, "carrier");
        } else {
            free(rb);
            EchoNestedResult got;
            CsilCodecArena *own = NULL;
            if (!csil_status_is_ok(resp.status)) {
                cases_add(cases, "echo-nested/success", false, "transport error");
            } else if (csil_decode_EchoNestedResult(resp.payload, resp.payload_len, &got, &own)) {
                cases_add(cases, "echo-nested/success", false, "decode");
            } else {
                cases_add(cases, "echo-nested/success", got.ok && nested_eq(&got.echo, &want),
                          "mismatch");
                csil_codec_arena_free(own);
            }
            csil_rpc_response_free(&resp);
        }
    }

    /* validate-constrained/success */
    {
        Constrained want = constrained_ok();
        uint8_t *rb = NULL;
        size_t rn = 0;
        csil_rpc_response resp;
        if (csil_encode_Constrained(&want, &rb, &rn)) {
            cases_add(cases, "validate-constrained/success", false, "encode");
        } else if (rpc_call(cl, "validate-constrained", rb, rn, &resp)) {
            free(rb);
            cases_add(cases, "validate-constrained/success", false, "carrier");
        } else {
            free(rb);
            if (resp.variant && strcmp(resp.variant, "ServiceError") == 0) {
                cases_add(cases, "validate-constrained/success", false, "unexpected service error");
            } else if (!csil_status_is_ok(resp.status)) {
                cases_add(cases, "validate-constrained/success", false, "transport error");
            } else {
                Constrained got;
                CsilCodecArena *own = NULL;
                if (csil_decode_Constrained(resp.payload, resp.payload_len, &got, &own)) {
                    cases_add(cases, "validate-constrained/success", false, "decode");
                } else {
                    cases_add(cases, "validate-constrained/success", constrained_eq(&got, &want),
                              "mismatch");
                    csil_codec_arena_free(own);
                }
            }
            csil_rpc_response_free(&resp);
        }
    }

    /* validate-constrained/failure: invalid input must come back as the typed
     * ServiceError arm (status 0, variant "ServiceError"). */
    {
        Constrained bad = constrained_bad();
        uint8_t *rb = NULL;
        size_t rn = 0;
        csil_rpc_response resp;
        if (csil_encode_Constrained(&bad, &rb, &rn)) {
            cases_add(cases, "validate-constrained/failure", false, "encode");
        } else if (rpc_call(cl, "validate-constrained", rb, rn, &resp)) {
            free(rb);
            cases_add(cases, "validate-constrained/failure", false, "carrier");
        } else {
            free(rb);
            bool is_service_err =
                csil_status_is_ok(resp.status) && resp.variant &&
                strcmp(resp.variant, "ServiceError") == 0;
            cases_add(cases, "validate-constrained/failure", is_service_err,
                      "expected service error");
            csil_rpc_response_free(&resp);
        }
    }

    csil_rpc_client_free(cl);
    csil_stream_carrier_dispose(&car);
}

/* --------------------------------------------------------------------------
 * Events
 * ------------------------------------------------------------------------ */

#define TICKS 3

static void send_event(csil_frame_carrier *car, const char *service, const char *name,
                       const uint8_t *payload, size_t len) {
    csil_event e;
    csil_event_init_verbose(&e, service, name, payload, len);
    uint8_t *out = NULL;
    size_t out_len = 0;
    if (csil_event_encode(&e, CSIL_PROFILE_VERBOSE, &out, &out_len) == CSIL_OK) {
        car->send_frame(car->userdata, out, out_len);
        csil_free(out);
    }
}

/* Receive and decode one verbose event. Returns false on EOF/error; on success
 * *ev is owned by the caller (free with csil_event_free). */
static bool recv_event(csil_frame_carrier *car, csil_event *ev) {
    uint8_t *frame = NULL;
    size_t flen = 0;
    if (car->recv_frame(car->userdata, &frame, &flen) != CSIL_OK || frame == NULL) {
        csil_free(frame);
        return false;
    }
    csil_err e = csil_event_decode(frame, flen, CSIL_PROFILE_VERBOSE, ev);
    csil_free(frame);
    return e == CSIL_OK;
}

static void events_serve_conn(int fd) {
    csil_stream st = {sock_read, sock_write, &fd};
    csil_frame_carrier car = csil_stream_carrier(&st, 0);

    /* Handshake: read $hello, reply $hello-ack. */
    csil_event ev;
    if (!recv_event(&car, &ev)) {
        csil_stream_carrier_dispose(&car);
        return;
    }
    if (!ev.event || strcmp(ev.event, CSIL_HELLO_NAME) != 0) {
        csil_event_free(&ev);
        csil_stream_carrier_dispose(&car);
        return;
    }
    csil_event_free(&ev);
    {
        csil_hello_ack ack = {1, "verbose", NULL, NULL};
        uint8_t *ab = NULL;
        size_t an = 0;
        if (csil_hello_ack_encode(&ack, &ab, &an) == CSIL_OK) {
            send_event(&car, NULL, CSIL_HELLO_ACK_NAME, ab, an);
            csil_free(ab);
        }
    }

    /* Push N ticks (on-tick / server push). */
    for (uint64_t seq = 0; seq < TICKS; seq++) {
        Tick t;
        t.seq = seq;
        uint8_t *tb = NULL;
        size_t tn = 0;
        if (csil_encode_Tick(&t, &tb, &tn) == 0) {
            send_event(&car, SERVICE, "on-tick", tb, tn);
            free(tb);
        }
    }

    /* React loop. */
    for (;;) {
        if (!recv_event(&car, &ev)) break;
        const char *method = ev.event ? ev.event : "";
        if (strcmp(method, CSIL_PING_NAME) == 0) {
            send_event(&car, NULL, CSIL_PONG_NAME, ev.payload, ev.payload_len);
        } else if (strcmp(method, CSIL_CLOSE_NAME) == 0) {
            csil_event_free(&ev);
            break;
        } else if (strcmp(method, "duplex") == 0) {
            Scalars s;
            CsilCodecArena *own = NULL;
            if (csil_decode_Scalars(ev.payload, ev.payload_len, &s, &own) == 0) {
                uint8_t *eb = NULL;
                size_t en = 0;
                if (csil_encode_Scalars(&s, &eb, &en) == 0) {
                    send_event(&car, SERVICE, "duplex", eb, en);
                    free(eb);
                }
                csil_codec_arena_free(own);
            }
        } else {
            send_event(&car, NULL, CSIL_ERROR_NAME, NULL, 0);
        }
        csil_event_free(&ev);
    }

    csil_stream_carrier_dispose(&car);
}

static void events_client_run(int fd, Cases *cases) {
    csil_stream st = {sock_read, sock_write, &fd};
    csil_frame_carrier car = csil_stream_carrier(&st, 0);

    /* Handshake. */
    uint64_t versions[] = {1};
    const char *profiles[] = {"verbose"};
    csil_hello hello = {versions, 1, profiles, 1, SERVICE, NULL, NULL};
    uint8_t *hb = NULL;
    size_t hn = 0;
    if (csil_hello_encode(&hello, &hb, &hn) == CSIL_OK) {
        send_event(&car, NULL, CSIL_HELLO_NAME, hb, hn);
        csil_free(hb);
    }
    csil_event ev;
    bool ack_ok = recv_event(&car, &ev) && ev.event && strcmp(ev.event, CSIL_HELLO_ACK_NAME) == 0;
    if (ack_ok) csil_event_free(&ev);
    cases_add(cases, "events/handshake", ack_ok, "no $hello-ack");

    /* on-tick: expect TICKS pushes with seq 0..TICKS-1. */
    bool tick_ok = true;
    char detail[128] = "";
    for (uint64_t expect = 0; expect < TICKS; expect++) {
        if (!recv_event(&car, &ev)) {
            tick_ok = false;
            snprintf(detail, sizeof(detail), "stream closed during ticks");
            break;
        }
        if (!ev.event || strcmp(ev.event, "on-tick") != 0) {
            tick_ok = false;
            snprintf(detail, sizeof(detail), "expected on-tick");
            csil_event_free(&ev);
            break;
        }
        Tick t;
        CsilCodecArena *own = NULL;
        if (csil_decode_Tick(ev.payload, ev.payload_len, &t, &own)) {
            tick_ok = false;
            snprintf(detail, sizeof(detail), "tick decode");
            csil_event_free(&ev);
            break;
        }
        if (t.seq != expect) {
            tick_ok = false;
            snprintf(detail, sizeof(detail), "tick seq %llu != %llu",
                     (unsigned long long)t.seq, (unsigned long long)expect);
        }
        csil_codec_arena_free(own);
        csil_event_free(&ev);
    }
    cases_add(cases, "on-tick/success", tick_ok, detail);

    /* duplex: send Scalars, expect echo. */
    {
        Scalars want = scalars_ok();
        uint8_t *eb = NULL;
        size_t en = 0;
        if (csil_encode_Scalars(&want, &eb, &en) == 0) {
            send_event(&car, SERVICE, "duplex", eb, en);
            free(eb);
        }
        if (recv_event(&car, &ev)) {
            if (ev.event && strcmp(ev.event, "duplex") == 0) {
                Scalars got;
                CsilCodecArena *own = NULL;
                if (csil_decode_Scalars(ev.payload, ev.payload_len, &got, &own)) {
                    cases_add(cases, "duplex/success", false, "decode");
                } else {
                    cases_add(cases, "duplex/success", scalars_eq(&got, &want), "echo mismatch");
                    csil_codec_arena_free(own);
                }
            } else {
                cases_add(cases, "duplex/success", false, "wrong event");
            }
            csil_event_free(&ev);
        } else {
            cases_add(cases, "duplex/success", false, "no echo");
        }
    }

    /* unknown-method: send a bogus channel method, expect $error. */
    send_event(&car, SERVICE, "Bogus", NULL, 0);
    if (recv_event(&car, &ev)) {
        bool is_err = ev.event && strcmp(ev.event, CSIL_ERROR_NAME) == 0;
        cases_add(cases, "unknown-method/failure", is_err, "expected $error");
        csil_event_free(&ev);
    } else {
        cases_add(cases, "unknown-method/failure", false, "no $error");
    }

    send_event(&car, NULL, CSIL_CLOSE_NAME, NULL, 0);
    csil_stream_carrier_dispose(&car);
}

/* --------------------------------------------------------------------------
 * Datagrams (UDP + lib envelope codec, raw sendto/recvfrom).
 * ------------------------------------------------------------------------ */

#define OP_ECHO_SCALARS 0u
#define OP_ECHO_COLLECTIONS 1u
#define OP_ERROR_SENTINEL 255u

static void datagram_serve(int fd) {
    uint8_t buf[65536];
    for (;;) {
        struct sockaddr_in from;
        socklen_t fromlen = sizeof(from);
        ssize_t n = recvfrom(fd, buf, sizeof(buf), 0, (struct sockaddr *)&from, &fromlen);
        if (n < 0) continue;
        csil_datagram dg;
        if (csil_datagram_decode(buf, (size_t)n, &dg) != CSIL_OK) continue;

        uint8_t *payload = NULL;
        size_t payload_len = 0;
        uint64_t reply_op = OP_ERROR_SENTINEL;
        if (dg.op_ord == OP_ECHO_SCALARS) {
            Scalars s;
            CsilCodecArena *own = NULL;
            if (csil_decode_Scalars(dg.payload, dg.payload_len, &s, &own) == 0) {
                if (csil_encode_Scalars(&s, &payload, &payload_len) == 0) reply_op = OP_ECHO_SCALARS;
                csil_codec_arena_free(own);
            }
        } else if (dg.op_ord == OP_ECHO_COLLECTIONS) {
            Collections c;
            CsilCodecArena *own = NULL;
            if (csil_decode_Collections(dg.payload, dg.payload_len, &c, &own) == 0) {
                if (csil_encode_Collections(&c, &payload, &payload_len) == 0)
                    reply_op = OP_ECHO_COLLECTIONS;
                csil_codec_arena_free(own);
            }
        }

        csil_datagram reply;
        csil_datagram_init(&reply, reply_op, dg.seq, payload, payload_len);
        uint8_t *out = NULL;
        size_t out_len = 0;
        if (csil_datagram_encode(&reply, &out, &out_len) == CSIL_OK) {
            sendto(fd, out, out_len, 0, (struct sockaddr *)&from, fromlen);
            csil_free(out);
        }
        free(payload);
        csil_datagram_free(&dg);
    }
}

/* Send one datagram and decode the reply into *reply (free with csil_datagram_free
 * on success). Returns 0 on a successful round-trip. */
static int datagram_roundtrip(int fd, uint64_t op, uint64_t seq, const uint8_t *payload,
                              size_t payload_len, csil_datagram *reply) {
    csil_datagram d;
    csil_datagram_init(&d, op, seq, payload, payload_len);
    uint8_t *out = NULL;
    size_t out_len = 0;
    if (csil_datagram_encode(&d, &out, &out_len) != CSIL_OK) return -1;
    ssize_t w = send(fd, out, out_len, 0);
    csil_free(out);
    if (w < 0) return -1;
    uint8_t buf[65536];
    ssize_t n = recv(fd, buf, sizeof(buf), 0);
    if (n < 0) return -1;
    return csil_datagram_decode(buf, (size_t)n, reply) == CSIL_OK ? 0 : -1;
}

static void datagram_client_run(int fd, Cases *cases) {
    /* echo-scalars */
    {
        Scalars want = scalars_ok();
        uint8_t *pb = NULL;
        size_t pn = 0;
        csil_datagram reply;
        if (csil_encode_Scalars(&want, &pb, &pn)) {
            cases_add(cases, "echo-scalars/success", false, "encode");
        } else if (datagram_roundtrip(fd, OP_ECHO_SCALARS, 1, pb, pn, &reply)) {
            free(pb);
            cases_add(cases, "echo-scalars/success", false, "no reply");
        } else {
            free(pb);
            if (reply.op_ord != OP_ECHO_SCALARS) {
                cases_add(cases, "echo-scalars/success", false, "wrong op_ord");
            } else {
                Scalars got;
                CsilCodecArena *own = NULL;
                if (csil_decode_Scalars(reply.payload, reply.payload_len, &got, &own)) {
                    cases_add(cases, "echo-scalars/success", false, "decode");
                } else {
                    cases_add(cases, "echo-scalars/success", scalars_eq(&got, &want), "mismatch");
                    csil_codec_arena_free(own);
                }
            }
            csil_datagram_free(&reply);
        }
    }

    /* echo-collections */
    {
        Collections want = collections_ok();
        uint8_t *pb = NULL;
        size_t pn = 0;
        csil_datagram reply;
        if (csil_encode_Collections(&want, &pb, &pn)) {
            cases_add(cases, "echo-collections/success", false, "encode");
        } else if (datagram_roundtrip(fd, OP_ECHO_COLLECTIONS, 2, pb, pn, &reply)) {
            free(pb);
            cases_add(cases, "echo-collections/success", false, "no reply");
        } else {
            free(pb);
            if (reply.op_ord != OP_ECHO_COLLECTIONS) {
                cases_add(cases, "echo-collections/success", false, "wrong op_ord");
            } else {
                Collections got;
                CsilCodecArena *own = NULL;
                if (csil_decode_Collections(reply.payload, reply.payload_len, &got, &own)) {
                    cases_add(cases, "echo-collections/success", false, "decode");
                } else {
                    cases_add(cases, "echo-collections/success", collections_eq(&got, &want),
                              "mismatch");
                    csil_codec_arena_free(own);
                }
            }
            csil_datagram_free(&reply);
        }
    }

    /* bad-op-ord: an unknown op_ord must come back as the error sentinel. */
    {
        csil_datagram reply;
        if (datagram_roundtrip(fd, 99, 3, NULL, 0, &reply)) {
            cases_add(cases, "bad-op-ord/failure", false, "no reply");
        } else {
            cases_add(cases, "bad-op-ord/failure", reply.op_ord == OP_ERROR_SENTINEL,
                      "expected error sentinel");
            csil_datagram_free(&reply);
        }
    }
}

/* --------------------------------------------------------------------------
 * Socket setup + main
 * ------------------------------------------------------------------------ */

static void set_loopback_addr(struct sockaddr_in *addr, uint16_t port) {
    memset(addr, 0, sizeof(*addr));
    addr->sin_family = AF_INET;
    addr->sin_port = htons(port);
    addr->sin_addr.s_addr = inet_addr("127.0.0.1");
}

static int stream_server(uint16_t port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    /* SO_REUSEADDR so a rapid rebind across the sequential per-transport runs
     * is not blocked by a TIME_WAIT lingering on the just-freed port. */
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    struct sockaddr_in addr;
    set_loopback_addr(&addr, port);
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0 || listen(fd, 16) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int stream_connect(uint16_t port) {
    struct sockaddr_in addr;
    set_loopback_addr(&addr, port);
    for (int attempt = 0; attempt < 200; attempt++) {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd >= 0 && connect(fd, (struct sockaddr *)&addr, sizeof(addr)) == 0) return fd;
        if (fd >= 0) close(fd);
        struct timespec ts = {0, 15 * 1000 * 1000};
        nanosleep(&ts, NULL);
    }
    return -1;
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s <server|client> <rpc|events|datagrams> <port>\n", argv[0]);
        return 2;
    }
    const char *mode = argv[1];
    const char *transport = argv[2];
    uint16_t port = (uint16_t)strtol(argv[3], NULL, 10);

    int is_server = strcmp(mode, "server") == 0;
    int is_dgram = strcmp(transport, "datagrams") == 0;

    if (is_server && is_dgram) {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        if (fd < 0) return 1;
        int one = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
        struct sockaddr_in addr;
        set_loopback_addr(&addr, port);
        if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) return 1;
        printf("READY\n");
        fflush(stdout);
        datagram_serve(fd);
        return 0;
    }
    if (is_server) {
        int lfd = stream_server(port);
        if (lfd < 0) return 1;
        printf("READY\n");
        fflush(stdout);
        for (;;) {
            int cfd = accept(lfd, NULL, NULL);
            if (cfd < 0) continue;
            if (strcmp(transport, "rpc") == 0)
                rpc_serve_conn(cfd);
            else
                events_serve_conn(cfd);
            close(cfd);
        }
        return 0;
    }

    /* client */
    if (is_dgram) {
        /* Ephemeral local UDP port; the server replies to this recvfrom source. */
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        if (fd < 0) return 1;
        struct sockaddr_in srv;
        set_loopback_addr(&srv, port);
        if (connect(fd, (struct sockaddr *)&srv, sizeof(srv)) != 0) return 1;
        struct timeval tv = {3, 0};
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
        Cases cases;
        cases_init(&cases, "datagrams");
        datagram_client_run(fd, &cases);
        cases_emit(&cases);
        close(fd);
        return 0;
    }

    int fd = stream_connect(port);
    if (fd < 0) return 1;
    Cases cases;
    if (strcmp(transport, "rpc") == 0) {
        cases_init(&cases, "rpc");
        rpc_client_run(fd, &cases);
    } else {
        cases_init(&cases, "events");
        events_client_run(fd, &cases);
    }
    cases_emit(&cases);
    close(fd);
    return 0;
}
