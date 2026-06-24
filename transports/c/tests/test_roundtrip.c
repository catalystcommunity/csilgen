/* End-to-end exercises of the carriers, the RPC client/server seam, the
 * length-prefix stream framing, and the datagram sequence tracker. These drive
 * the synchronous, threads-free API the way a host would, on the in-memory
 * carriers, with no socket. */
#include "csil/csil.h"

#include <stdio.h>
#include <stdlib.h>
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

static const uint8_t EMPTY_MAP[] = {0xa0}; /* CBOR {} - a stand-in payload */

/* A handler that always replies with the same variant, echoing the request
 * payload back as the reply payload. */
static void echo_handler(void *ctx, const csil_rpc_request *req,
                         csil_rpc_outcome *out) {
    (void)ctx;
    csil_rpc_outcome_reply(out, "EchoResponse", req->payload, req->payload_len);
}

static void unknown_op_handler(void *ctx, const csil_rpc_request *req,
                               csil_rpc_outcome *out) {
    (void)ctx;
    (void)req;
    csil_rpc_outcome_transport(
        out, csil_status_from_code(CSIL_STATUS_UNKNOWN_SERVICE_OR_OP_CODE),
        "no such op");
}

static void test_server(void) {
    csil_loopback_frame *lb = csil_loopback_frame_new();
    csil_frame_carrier carrier = csil_loopback_frame_carrier(lb);

    csil_rpc_request req;
    csil_rpc_request_init(&req, "Echo", "say", EMPTY_MAP, sizeof EMPTY_MAP);
    req.has_id = true;
    req.id = 42;
    uint8_t *frame = NULL;
    size_t frame_len = 0;
    CHECK(csil_rpc_request_encode(&req, &frame, &frame_len) == CSIL_OK,
          "encode request");
    CHECK(csil_loopback_frame_push_inbound(lb, frame, frame_len) == CSIL_OK,
          "push inbound");
    csil_free(frame);

    csil_rpc_server *srv = csil_rpc_server_new(&carrier);
    bool served = false;
    CHECK(csil_rpc_server_serve_one(srv, echo_handler, NULL, &served) == CSIL_OK,
          "serve_one");
    CHECK(served, "served one request");

    uint8_t *out = NULL;
    size_t out_len = 0;
    csil_loopback_frame_take_outbound(lb, &out, &out_len);
    CHECK(out != NULL, "server produced a response");
    csil_rpc_response resp;
    CHECK(csil_rpc_response_decode(out, out_len, &resp) == CSIL_OK,
          "decode response");
    CHECK(resp.has_id && resp.id == 42, "response echoes the id");
    CHECK(csil_status_is_ok(resp.status), "reply status is ok");
    CHECK(resp.variant && strcmp(resp.variant, "EchoResponse") == 0,
          "reply names the variant");
    CHECK(resp.payload_len == sizeof EMPTY_MAP, "reply payload round-trips");
    csil_rpc_response_free(&resp);
    csil_free(out);

    /* A second serve at a drained queue is a clean end of stream. */
    served = true;
    CHECK(csil_rpc_server_serve_one(srv, echo_handler, NULL, &served) == CSIL_OK,
          "serve_one at EOS");
    CHECK(!served, "clean end of stream sets served=false");

    csil_rpc_server_free(srv);
    csil_loopback_frame_free(lb);
}

static void test_server_transport_error(void) {
    csil_loopback_frame *lb = csil_loopback_frame_new();
    csil_frame_carrier carrier = csil_loopback_frame_carrier(lb);

    csil_rpc_request req;
    csil_rpc_request_init(&req, "Echo", "nope", EMPTY_MAP, sizeof EMPTY_MAP);
    uint8_t *frame = NULL;
    size_t frame_len = 0;
    CHECK(csil_rpc_request_encode(&req, &frame, &frame_len) == CSIL_OK, "encode");
    csil_loopback_frame_push_inbound(lb, frame, frame_len);
    csil_free(frame);

    csil_rpc_server *srv = csil_rpc_server_new(&carrier);
    bool served = false;
    csil_rpc_server_serve_one(srv, unknown_op_handler, NULL, &served);
    uint8_t *out = NULL;
    size_t out_len = 0;
    csil_loopback_frame_take_outbound(lb, &out, &out_len);
    csil_rpc_response resp;
    CHECK(csil_rpc_response_decode(out, out_len, &resp) == CSIL_OK, "decode");
    CHECK(csil_rpc_response_is_transport_error(&resp), "is transport error");
    CHECK(resp.status.code == CSIL_STATUS_UNKNOWN_SERVICE_OR_OP_CODE,
          "carries the status code");
    CHECK(resp.error && strcmp(resp.error, "no such op") == 0, "carries message");
    csil_rpc_response_free(&resp);
    csil_free(out);
    csil_rpc_server_free(srv);
    csil_loopback_frame_free(lb);
}

static void test_client(void) {
    csil_loopback_frame *lb = csil_loopback_frame_new();
    csil_frame_carrier carrier = csil_loopback_frame_carrier(lb);

    /* Canned reply the client will receive. */
    csil_rpc_response reply;
    csil_rpc_response_init_ok(&reply, "Pong", EMPTY_MAP, sizeof EMPTY_MAP);
    reply.has_id = true;
    reply.id = 1;
    uint8_t *replybytes = NULL;
    size_t reply_len = 0;
    CHECK(csil_rpc_response_encode(&reply, &replybytes, &reply_len) == CSIL_OK,
          "encode reply");
    csil_loopback_frame_push_inbound(lb, replybytes, reply_len);
    csil_free(replybytes);

    csil_rpc_client *cli = csil_rpc_client_new(&carrier, true);
    csil_rpc_response resp;
    CHECK(csil_rpc_client_call(cli, "Ping", "ping", EMPTY_MAP, sizeof EMPTY_MAP,
                               NULL, &resp) == CSIL_OK,
          "client call");
    CHECK(!csil_rpc_response_is_transport_error(&resp), "ok response");
    CHECK(resp.variant && strcmp(resp.variant, "Pong") == 0, "variant");
    csil_rpc_response_free(&resp);

    /* The request the client emitted should carry a correlation id of 1. */
    uint8_t *sent = NULL;
    size_t sent_len = 0;
    csil_loopback_frame_take_outbound(lb, &sent, &sent_len);
    CHECK(sent != NULL, "client sent a request");
    csil_rpc_request decoded;
    CHECK(csil_rpc_request_decode(sent, sent_len, &decoded) == CSIL_OK, "decode");
    CHECK(strcmp(decoded.service, "Ping") == 0, "service");
    CHECK(strcmp(decoded.op, "ping") == 0, "op");
    CHECK(decoded.has_id && decoded.id == 1, "multiplexed id assigned");
    csil_rpc_request_free(&decoded);
    csil_free(sent);

    csil_rpc_client_free(cli);
    csil_loopback_frame_free(lb);
}

/* ---- length-prefix stream carrier over an in-memory byte buffer ----------- */

typedef struct membuf {
    uint8_t *data;
    size_t len;
    size_t cap;
    size_t rpos;
} membuf;

static long membuf_read(void *ud, uint8_t *buf, size_t len) {
    membuf *m = ud;
    size_t avail = m->len - m->rpos;
    if (avail == 0) {
        return 0; /* EOF */
    }
    size_t n = len < avail ? len : avail;
    memcpy(buf, m->data + m->rpos, n);
    m->rpos += n;
    return (long)n;
}

static int membuf_write(void *ud, const uint8_t *buf, size_t len) {
    membuf *m = ud;
    if (m->len + len > m->cap) {
        size_t cap = m->cap ? m->cap : 64;
        while (cap < m->len + len) {
            cap *= 2;
        }
        uint8_t *p = realloc(m->data, cap);
        if (!p) {
            return -1;
        }
        m->data = p;
        m->cap = cap;
    }
    memcpy(m->data + m->len, buf, len);
    m->len += len;
    return 0;
}

static void test_stream_framing(void) {
    membuf m;
    memset(&m, 0, sizeof(m));
    csil_stream stream = {membuf_read, membuf_write, &m};
    csil_frame_carrier carrier = csil_stream_carrier(&stream, 0);
    CHECK(carrier.send_frame != NULL, "stream carrier constructed");

    const uint8_t f1[] = {0x01, 0x02, 0x03};
    const uint8_t f2[] = {0xaa};
    CHECK(carrier.send_frame(carrier.userdata, f1, sizeof f1) == CSIL_OK, "send f1");
    CHECK(carrier.send_frame(carrier.userdata, f2, sizeof f2) == CSIL_OK, "send f2");
    /* 4-byte big-endian prefix + payload, twice. */
    CHECK(m.len == 4 + sizeof f1 + 4 + sizeof f2, "framed length");
    CHECK(m.data[0] == 0 && m.data[3] == sizeof f1, "big-endian length prefix");

    uint8_t *got = NULL;
    size_t got_len = 0;
    CHECK(carrier.recv_frame(carrier.userdata, &got, &got_len) == CSIL_OK, "recv f1");
    CHECK(got_len == sizeof f1 && memcmp(got, f1, sizeof f1) == 0, "f1 round-trip");
    csil_free(got);
    CHECK(carrier.recv_frame(carrier.userdata, &got, &got_len) == CSIL_OK, "recv f2");
    CHECK(got_len == sizeof f2 && memcmp(got, f2, sizeof f2) == 0, "f2 round-trip");
    csil_free(got);
    /* A clean EOF at the boundary returns NULL with CSIL_OK. */
    CHECK(carrier.recv_frame(carrier.userdata, &got, &got_len) == CSIL_OK, "recv EOS");
    CHECK(got == NULL, "clean end of stream");

    csil_stream_carrier_dispose(&carrier);
    free(m.data);
}

static void test_frame_too_large(void) {
    membuf m;
    memset(&m, 0, sizeof(m));
    csil_stream stream = {membuf_read, membuf_write, &m};
    csil_frame_carrier carrier = csil_stream_carrier(&stream, 8);
    uint8_t big[16] = {0};
    CHECK(carrier.send_frame(carrier.userdata, big, sizeof big) ==
              CSIL_ERR_FRAME_TOO_LARGE,
          "oversize frame rejected before sending");
    csil_stream_carrier_dispose(&carrier);
    free(m.data);
}

static void test_seq_tracker(void) {
    csil_seq_tracker t;
    csil_seq_tracker_init(&t);
    csil_seq_event e;

    e = csil_seq_tracker_observe(&t, 1, false, 0);
    CHECK(e.kind == CSIL_SEQ_FIRST, "first datagram");
    e = csil_seq_tracker_observe(&t, 2, false, 0);
    CHECK(e.kind == CSIL_SEQ_ADVANCED && e.gap == 0, "contiguous advance");
    e = csil_seq_tracker_observe(&t, 5, false, 0);
    CHECK(e.kind == CSIL_SEQ_ADVANCED && e.gap == 2, "gap of two");
    e = csil_seq_tracker_observe(&t, 3, false, 0);
    CHECK(e.kind == CSIL_SEQ_LATE_OR_DUPLICATE, "late/duplicate");
    e = csil_seq_tracker_observe(&t, 0, false, 0);
    CHECK(e.kind == CSIL_SEQ_ADVANCED && e.gap == 0, "unsequenced advance");

    /* A restart requires a *prior* epoch that then changes (matching the Go/Rust
     * reference): going from no-epoch to epoch is not a restart. */
    csil_seq_tracker te;
    csil_seq_tracker_init(&te);
    e = csil_seq_tracker_observe(&te, 1, true, 5);
    CHECK(e.kind == CSIL_SEQ_FIRST, "first with epoch");
    e = csil_seq_tracker_observe(&te, 2, true, 5);
    CHECK(e.kind == CSIL_SEQ_ADVANCED && e.gap == 0, "advance within epoch");
    e = csil_seq_tracker_observe(&te, 3, true, 7);
    CHECK(e.kind == CSIL_SEQ_RESTART, "epoch change is a restart");
}

static void test_decode_rejects_garbage(void) {
    const uint8_t trailing[] = {0xa0, 0xff};   /* valid map then a stray byte */
    csil_rpc_request req;
    CHECK(csil_rpc_request_decode(trailing, sizeof trailing, &req) != CSIL_OK,
          "trailing bytes rejected");
    const uint8_t truncated[] = {0x62, 0x6f}; /* claims 2-byte text, has 1 */
    CHECK(csil_rpc_request_decode(truncated, sizeof truncated, &req) != CSIL_OK,
          "truncated input rejected");
}

int main(void) {
    test_server();
    test_server_transport_error();
    test_client();
    test_stream_framing();
    test_frame_too_large();
    test_seq_tracker();
    test_decode_rejects_garbage();
    printf("roundtrip: %d checks, %d failures\n", g_checks, g_failures);
    return g_failures == 0 ? 0 : 1;
}
