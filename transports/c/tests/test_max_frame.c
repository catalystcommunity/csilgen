/* The configurable max-frame guard (conventions doc section 5): a host sets the
 * limit up or down through csil_stream_carrier, the limit applies to reads and
 * writes alike, an oversized inbound length is rejected before allocation, and an
 * invalid limit yields an unusable carrier at construction rather than failing on
 * the first frame. */
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

/* A growable in-memory byte stream. read_calls counts bytes handed out, so a test
 * can prove the guard fires before a frame body is ever pulled off the wire. */
typedef struct membuf {
    uint8_t *data;
    size_t len;
    size_t cap;
    size_t rpos;
    size_t bytes_read;
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
    m->bytes_read += n;
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

static void test_default_limit_accepts_frame_below_it(void) {
    membuf m;
    memset(&m, 0, sizeof(m));
    csil_stream stream = {membuf_read, membuf_write, &m};
    csil_frame_carrier c = csil_stream_carrier(&stream, 0); /* 0 selects the default */
    CHECK(c.send_frame != NULL, "default carrier constructed");

    uint8_t body[1024];
    memset(body, 0xAB, sizeof body);
    CHECK(c.send_frame(c.userdata, body, sizeof body) == CSIL_OK,
          "frame under the default limit sends");

    uint8_t *got = NULL;
    size_t got_len = 0;
    CHECK(c.recv_frame(c.userdata, &got, &got_len) == CSIL_OK, "frame reads back");
    CHECK(got_len == sizeof body && memcmp(got, body, sizeof body) == 0,
          "round trip preserved the frame");
    csil_free(got);
    csil_stream_carrier_dispose(&c);
    free(m.data);
}

static void test_default_limit_rejects_frame_above_it(void) {
    membuf m;
    memset(&m, 0, sizeof(m));
    csil_stream stream = {membuf_read, membuf_write, &m};
    csil_frame_carrier c = csil_stream_carrier(&stream, 0);

    /* One byte over the default guard. Allocated rather than declared so the test
     * does not put 16 MiB on the stack. */
    size_t n = CSIL_MAX_FRAME_DEFAULT + 1;
    uint8_t *big = calloc(1, n);
    CHECK(big != NULL, "test allocation");
    if (big) {
        CHECK(c.send_frame(c.userdata, big, n) == CSIL_ERR_FRAME_TOO_LARGE,
              "oversized frame rejected by the default limit");
        CHECK(m.len == 0, "a rejected frame must not put bytes on the wire");
        free(big);
    }
    csil_stream_carrier_dispose(&c);
    free(m.data);
}

static void test_larger_custom_limit_accepts_what_default_rejects(void) {
    membuf m;
    memset(&m, 0, sizeof(m));
    csil_stream stream = {membuf_read, membuf_write, &m};
    csil_frame_carrier c =
        csil_stream_carrier(&stream, CSIL_MAX_FRAME_DEFAULT + 4096);
    CHECK(c.send_frame != NULL, "raised-limit carrier constructed");

    size_t n = CSIL_MAX_FRAME_DEFAULT + 1;
    uint8_t *big = calloc(1, n);
    CHECK(big != NULL, "test allocation");
    if (big) {
        CHECK(c.send_frame(c.userdata, big, n) == CSIL_OK,
              "raised limit accepts what the default rejects");
        uint8_t *got = NULL;
        size_t got_len = 0;
        CHECK(c.recv_frame(c.userdata, &got, &got_len) == CSIL_OK, "reads back");
        CHECK(got_len == n, "full length round-tripped");
        csil_free(got);
        free(big);
    }
    csil_stream_carrier_dispose(&c);
    free(m.data);
}

static void test_smaller_custom_limit_rejects_what_default_accepts(void) {
    membuf m;
    memset(&m, 0, sizeof(m));
    csil_stream stream = {membuf_read, membuf_write, &m};
    csil_frame_carrier c = csil_stream_carrier(&stream, 64);

    uint8_t body[1024];
    memset(body, 0xCD, sizeof body);
    CHECK(c.send_frame(c.userdata, body, sizeof body) == CSIL_ERR_FRAME_TOO_LARGE,
          "lowered limit rejects a frame the default would accept");
    csil_stream_carrier_dispose(&c);
    free(m.data);
}

static void test_oversized_incoming_length_rejected_before_allocation(void) {
    /* A prefix claiming ~4 GiB followed by no body: if the guard ran after the read
     * this would allocate; it must fail on the prefix alone. */
    membuf m;
    memset(&m, 0, sizeof(m));
    uint8_t prefix[4] = {0xFF, 0xFF, 0xFF, 0xFF};
    csil_stream stream = {membuf_read, membuf_write, &m};
    CHECK(membuf_write(&m, prefix, sizeof prefix) == 0, "seed the prefix");

    csil_frame_carrier c = csil_stream_carrier(&stream, 4096);
    uint8_t *got = NULL;
    size_t got_len = 0;
    CHECK(c.recv_frame(c.userdata, &got, &got_len) == CSIL_ERR_FRAME_TOO_LARGE,
          "oversized inbound length rejected");
    CHECK(m.bytes_read == 4, "guard must fire on the 4-byte prefix alone");
    CHECK(got == NULL, "no frame handed back");
    csil_stream_carrier_dispose(&c);
    free(m.data);
}

static void test_invalid_limits_rejected_at_construction(void) {
    membuf m;
    memset(&m, 0, sizeof(m));
    csil_stream stream = {membuf_read, membuf_write, &m};

    /* size_t is unsigned, so "negative" arrives as a huge value; both it and any
     * limit past the portable ceiling must yield an unusable carrier. SIZE_MAX
     * stands in for the wrapped -1 a caller would pass. */
    size_t invalid[] = {CSIL_MAX_FRAME_LIMIT + 1, (size_t)-1, (size_t)-4096};
    for (size_t i = 0; i < sizeof invalid / sizeof invalid[0]; i++) {
        csil_frame_carrier c = csil_stream_carrier(&stream, invalid[i]);
        CHECK(c.send_frame == NULL && c.recv_frame == NULL,
              "limit %zu must yield an unusable carrier", invalid[i]);
        csil_stream_carrier_dispose(&c);
    }
    CHECK(csil_validate_max_frame(0) == 0, "0 is not a valid explicit limit");
    free(m.data);
}

static void test_boundary_limits_accepted(void) {
    membuf m;
    memset(&m, 0, sizeof(m));
    csil_stream stream = {membuf_read, membuf_write, &m};

    size_t valid[] = {1, CSIL_MAX_FRAME_DEFAULT, CSIL_MAX_FRAME_LIMIT};
    for (size_t i = 0; i < sizeof valid / sizeof valid[0]; i++) {
        CHECK(csil_validate_max_frame(valid[i]) == 1, "limit %zu is valid", valid[i]);
        csil_frame_carrier c = csil_stream_carrier(&stream, valid[i]);
        CHECK(c.send_frame != NULL, "limit %zu builds a usable carrier", valid[i]);
        csil_stream_carrier_dispose(&c);
    }
    free(m.data);
}

int main(void) {
    test_default_limit_accepts_frame_below_it();
    test_default_limit_rejects_frame_above_it();
    test_larger_custom_limit_accepts_what_default_rejects();
    test_smaller_custom_limit_rejects_what_default_accepts();
    test_oversized_incoming_length_rejected_before_allocation();
    test_invalid_limits_rejected_at_construction();
    test_boundary_limits_accepted();

    printf("max-frame: %d checks, %d failures\n", g_checks, g_failures);
    return g_failures == 0 ? 0 : 1;
}
