/* Verify the C reference library against the checked-in conformance vectors.
 *
 * This both guards the C encoders/decoders against drift and demonstrates the
 * contract every reference library follows: reconstruct each vector's envelope
 * from its language-neutral `input` (flattened into vectors.gen.h), assert
 * encode -> `hex`, and assert decode(`hex`) -> the same envelope. Ported from
 * transports/rust/tests/conformance.rs and transports/go/conformance_test.go. */
#include "csil/csil.h"

#include "vectors.gen.h"

#include <stdio.h>
#include <string.h>

static int g_failures = 0;
static int g_checks = 0;

#define CHECK(cond, ...)                                                        \
    do {                                                                        \
        g_checks++;                                                             \
        if (!(cond)) {                                                          \
            g_failures++;                                                       \
            fprintf(stderr, "FAIL %s:%d: ", __FILE__, __LINE__);               \
            fprintf(stderr, __VA_ARGS__);                                       \
            fprintf(stderr, "\n");                                              \
        }                                                                       \
    } while (0)

static bool bytes_eq(const uint8_t *a, size_t an, const uint8_t *b, size_t bn) {
    if (an != bn) {
        return false;
    }
    return an == 0 || memcmp(a, b, an) == 0;
}

static void hexdump(const char *label, const uint8_t *p, size_t n) {
    fprintf(stderr, "  %s (%zu): ", label, n);
    for (size_t i = 0; i < n; i++) {
        fprintf(stderr, "%02x", p[i]);
    }
    fprintf(stderr, "\n");
}

/* Assert that `got` matches the vector hex; dump both on mismatch. */
static void expect_hex(const char *name, const uint8_t *got, size_t got_len,
                       const uint8_t *want, size_t want_len) {
    bool ok = bytes_eq(got, got_len, want, want_len);
    CHECK(ok, "%s encode mismatch", name);
    if (!ok) {
        hexdump("got ", got, got_len);
        hexdump("want", want, want_len);
    }
}

static void test_rpc(void) {
    for (size_t i = 0; i < RPC_VECTOR_COUNT; i++) {
        const rpc_vector *v = &RPC_VECTORS[i];
        uint8_t *enc = NULL;
        size_t enc_len = 0;
        if (strcmp(v->kind, "request") == 0) {
            csil_rpc_request req;
            csil_rpc_request_init(&req, v->service, v->op, v->payload,
                                  v->payload_len);
            req.has_id = v->has_id;
            req.id = v->id;
            req.auth = v->auth;
            CHECK(csil_rpc_request_encode(&req, &enc, &enc_len) == CSIL_OK,
                  "%s encode error", v->name);
            expect_hex(v->name, enc, enc_len, v->hex, v->hex_len);
            csil_rpc_request dec;
            CHECK(csil_rpc_request_decode(v->hex, v->hex_len, &dec) == CSIL_OK,
                  "%s decode error", v->name);
            CHECK(csil_rpc_request_equal(&dec, &req), "%s decode mismatch", v->name);
            csil_rpc_request_free(&dec);
        } else if (strcmp(v->kind, "response") == 0) {
            csil_rpc_response resp;
            memset(&resp, 0, sizeof(resp));
            resp.has_id = v->has_id;
            resp.id = v->id;
            resp.status = csil_status_from_code(v->status);
            resp.variant = v->variant;
            resp.error = v->error;
            resp.payload = v->payload;
            resp.payload_len = v->payload_len;
            CHECK(csil_rpc_response_encode(&resp, &enc, &enc_len) == CSIL_OK,
                  "%s encode error", v->name);
            expect_hex(v->name, enc, enc_len, v->hex, v->hex_len);
            csil_rpc_response dec;
            CHECK(csil_rpc_response_decode(v->hex, v->hex_len, &dec) == CSIL_OK,
                  "%s decode error", v->name);
            CHECK(csil_rpc_response_equal(&dec, &resp), "%s decode mismatch",
                  v->name);
            csil_rpc_response_free(&dec);
        } else if (strcmp(v->kind, "push") == 0) {
            csil_rpc_push push;
            csil_rpc_push_init(&push, v->service, v->event, v->payload,
                               v->payload_len);
            CHECK(csil_rpc_push_encode(&push, &enc, &enc_len) == CSIL_OK,
                  "%s encode error", v->name);
            expect_hex(v->name, enc, enc_len, v->hex, v->hex_len);
            csil_rpc_push dec;
            CHECK(csil_rpc_push_decode(v->hex, v->hex_len, &dec) == CSIL_OK,
                  "%s decode error", v->name);
            CHECK(csil_rpc_push_equal(&dec, &push), "%s decode mismatch", v->name);
            csil_rpc_push_free(&dec);
        } else {
            CHECK(false, "%s unknown rpc kind %s", v->name, v->kind);
        }
        csil_free(enc);
    }
}

/* Control payloads only check encode (matching the Go reference) but additionally
 * decode and re-encode to prove the decoder populated every field. */
static void roundtrip_control(const char *name, uint8_t *enc, size_t enc_len,
                              const events_vector *v,
                              csil_err (*reencode)(const uint8_t *, size_t,
                                                   uint8_t **, size_t *)) {
    expect_hex(name, enc, enc_len, v->hex, v->hex_len);
    uint8_t *re = NULL;
    size_t re_len = 0;
    CHECK(reencode(enc, enc_len, &re, &re_len) == CSIL_OK, "%s reencode error",
          name);
    expect_hex(name, re, re_len, v->hex, v->hex_len);
    csil_free(re);
}

static csil_err hello_reencode(const uint8_t *b, size_t n, uint8_t **out,
                               size_t *out_len) {
    csil_hello h;
    csil_err e = csil_hello_decode(b, n, &h);
    if (e) {
        return e;
    }
    e = csil_hello_encode(&h, out, out_len);
    csil_hello_free(&h);
    return e;
}

static csil_err hello_ack_reencode(const uint8_t *b, size_t n, uint8_t **out,
                                   size_t *out_len) {
    csil_hello_ack a;
    csil_err e = csil_hello_ack_decode(b, n, &a);
    if (e) {
        return e;
    }
    e = csil_hello_ack_encode(&a, out, out_len);
    csil_hello_ack_free(&a);
    return e;
}

static csil_err heartbeat_reencode(const uint8_t *b, size_t n, uint8_t **out,
                                   size_t *out_len) {
    csil_heartbeat h;
    csil_err e = csil_heartbeat_decode(b, n, &h);
    if (e) {
        return e;
    }
    e = csil_heartbeat_encode(&h, out, out_len);
    csil_heartbeat_free(&h);
    return e;
}

static csil_err close_reencode(const uint8_t *b, size_t n, uint8_t **out,
                               size_t *out_len) {
    csil_close c;
    csil_err e = csil_close_decode(b, n, &c);
    if (e) {
        return e;
    }
    e = csil_close_encode(&c, out, out_len);
    csil_close_free(&c);
    return e;
}

static void test_events(void) {
    for (size_t i = 0; i < EVENTS_VECTOR_COUNT; i++) {
        const events_vector *v = &EVENTS_VECTORS[i];
        uint8_t *enc = NULL;
        size_t enc_len = 0;
        if (v->control) {
            if (strcmp(v->control, "hello") == 0) {
                csil_hello h;
                memset(&h, 0, sizeof(h));
                h.versions = v->versions;
                h.versions_len = v->versions_len;
                h.profiles = v->profiles;
                h.profiles_len = v->profiles_len;
                h.service = v->service;
                h.auth = v->auth;
                CHECK(csil_hello_encode(&h, &enc, &enc_len) == CSIL_OK,
                      "%s encode error", v->name);
                roundtrip_control(v->name, enc, enc_len, v, hello_reencode);
            } else if (strcmp(v->control, "hello_ack") == 0) {
                csil_hello_ack a;
                memset(&a, 0, sizeof(a));
                a.v = v->v;
                a.profile = v->profile;
                a.session = v->session;
                CHECK(csil_hello_ack_encode(&a, &enc, &enc_len) == CSIL_OK,
                      "%s encode error", v->name);
                roundtrip_control(v->name, enc, enc_len, v, hello_ack_reencode);
            } else if (strcmp(v->control, "ping") == 0) {
                csil_heartbeat h;
                memset(&h, 0, sizeof(h));
                h.nonce = v->nonce;
                h.has_at = v->has_at;
                h.at = v->at;
                CHECK(csil_heartbeat_encode(&h, &enc, &enc_len) == CSIL_OK,
                      "%s encode error", v->name);
                roundtrip_control(v->name, enc, enc_len, v, heartbeat_reencode);
            } else if (strcmp(v->control, "close") == 0) {
                csil_close c;
                memset(&c, 0, sizeof(c));
                c.status = csil_status_from_code(v->status);
                c.reason = v->reason;
                CHECK(csil_close_encode(&c, &enc, &enc_len) == CSIL_OK,
                      "%s encode error", v->name);
                roundtrip_control(v->name, enc, enc_len, v, close_reencode);
            } else {
                CHECK(false, "%s unknown control %s", v->name, v->control);
            }
            csil_free(enc);
            continue;
        }

        csil_profile profile;
        CHECK(csil_profile_parse(v->profile, &profile), "%s bad profile %s",
              v->name, v->profile);
        csil_event ev;
        if (profile == CSIL_PROFILE_VERBOSE) {
            csil_event_init_verbose(&ev, v->service, v->event, v->payload,
                                    v->payload_len);
        } else {
            csil_event_init_compact(&ev, v->service_ord, v->op_ord, v->payload,
                                    v->payload_len);
        }
        ev.has_id = v->has_id;
        ev.id = v->id;
        CHECK(csil_event_encode(&ev, profile, &enc, &enc_len) == CSIL_OK,
              "%s encode error", v->name);
        expect_hex(v->name, enc, enc_len, v->hex, v->hex_len);
        csil_event dec;
        CHECK(csil_event_decode(v->hex, v->hex_len, profile, &dec) == CSIL_OK,
              "%s decode error", v->name);
        CHECK(csil_event_equal(&dec, &ev), "%s decode mismatch", v->name);
        csil_event_free(&dec);
        csil_free(enc);
    }
}

static void test_datagrams(void) {
    for (size_t i = 0; i < DATAGRAM_VECTOR_COUNT; i++) {
        const datagram_vector *v = &DATAGRAM_VECTORS[i];
        uint8_t *enc = NULL;
        size_t enc_len = 0;
        if (strcmp(v->profile, "cbor-array") == 0) {
            csil_datagram d;
            csil_datagram_init(&d, v->op_ord, v->seq, v->payload, v->payload_len);
            CHECK(csil_datagram_encode(&d, &enc, &enc_len) == CSIL_OK,
                  "%s encode error", v->name);
            expect_hex(v->name, enc, enc_len, v->hex, v->hex_len);
            csil_datagram dec;
            CHECK(csil_datagram_decode(v->hex, v->hex_len, &dec) == CSIL_OK,
                  "%s decode error", v->name);
            CHECK(csil_datagram_equal(&dec, &d), "%s decode mismatch", v->name);
            csil_datagram_free(&dec);
        } else if (strcmp(v->profile, "compact-header") == 0) {
            csil_compact_datagram d;
            csil_compact_datagram_init(&d, (uint8_t)v->op_ord, (uint16_t)v->seq,
                                       v->body, v->body_len);
            if (v->has_epoch) {
                csil_compact_datagram_set_epoch(&d, (uint8_t)v->epoch);
            }
            CHECK(csil_compact_datagram_encode(&d, &enc, &enc_len) == CSIL_OK,
                  "%s encode error", v->name);
            expect_hex(v->name, enc, enc_len, v->hex, v->hex_len);
            csil_compact_datagram dec;
            CHECK(csil_compact_datagram_decode(v->hex, v->hex_len, &dec) == CSIL_OK,
                  "%s decode error", v->name);
            CHECK(csil_compact_datagram_equal(&dec, &d), "%s decode mismatch",
                  v->name);
            csil_compact_datagram_free(&dec);
        } else {
            CHECK(false, "%s unknown datagram profile %s", v->name, v->profile);
        }
        csil_free(enc);
    }
}

int main(void) {
    test_rpc();
    test_events();
    test_datagrams();
    printf("conformance: %d checks, %d failures\n", g_checks, g_failures);
    return g_failures == 0 ? 0 : 1;
}
