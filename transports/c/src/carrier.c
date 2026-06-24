#include "csil/csil_carrier.h"

#include <stdlib.h>
#include <string.h>

/* ---- length-prefixed stream carrier --------------------------------------- */

typedef struct stream_state {
    csil_stream stream;
    size_t max_frame;
} stream_state;

/* Read exactly len bytes. Returns CSIL_OK on a full read, a special "clean EOF
 * at boundary" via *got_any (false when zero bytes were read before EOF), or a
 * carrier error. */
static csil_err read_full(const csil_stream *s, uint8_t *buf, size_t len,
                          bool *got_any) {
    size_t off = 0;
    *got_any = false;
    while (off < len) {
        long n = s->read(s->userdata, buf + off, len - off);
        if (n < 0) {
            return CSIL_ERR_CARRIER;
        }
        if (n == 0) {
            /* EOF: clean only if it lands exactly on a frame boundary. */
            return off == 0 ? CSIL_OK : CSIL_ERR_CARRIER;
        }
        off += (size_t)n;
        *got_any = true;
    }
    return CSIL_OK;
}

static csil_err stream_send(void *userdata, const uint8_t *data, size_t len) {
    stream_state *st = userdata;
    if (len > st->max_frame) {
        return CSIL_ERR_FRAME_TOO_LARGE;
    }
    uint8_t prefix[4];
    prefix[0] = (uint8_t)(len >> 24);
    prefix[1] = (uint8_t)(len >> 16);
    prefix[2] = (uint8_t)(len >> 8);
    prefix[3] = (uint8_t)len;
    if (st->stream.write(st->stream.userdata, prefix, 4) != 0) {
        return CSIL_ERR_CARRIER;
    }
    if (len && st->stream.write(st->stream.userdata, data, len) != 0) {
        return CSIL_ERR_CARRIER;
    }
    return CSIL_OK;
}

static csil_err stream_recv(void *userdata, uint8_t **out, size_t *out_len) {
    stream_state *st = userdata;
    *out = NULL;
    *out_len = 0;
    uint8_t lenbuf[4];
    size_t off = 0;
    bool saw_partial = false;
    while (off < 4) {
        long n = st->stream.read(st->stream.userdata, lenbuf + off, 4 - off);
        if (n < 0) {
            return CSIL_ERR_CARRIER;
        }
        if (n == 0) {
            /* A clean EOF before any prefix byte is an orderly end of stream. */
            return (off == 0 && !saw_partial) ? CSIL_OK : CSIL_ERR_CARRIER;
        }
        off += (size_t)n;
        saw_partial = true;
    }
    /* Compare the prefix as an unsigned value before narrowing to size_t, so a
     * length with the high bit set can never slip past the guard. */
    uint64_t length = ((uint64_t)lenbuf[0] << 24) | ((uint64_t)lenbuf[1] << 16) |
                      ((uint64_t)lenbuf[2] << 8) | (uint64_t)lenbuf[3];
    if (length > (uint64_t)st->max_frame) {
        return CSIL_ERR_FRAME_TOO_LARGE;
    }
    uint8_t *buf = malloc(length ? (size_t)length : 1);
    if (!buf) {
        return CSIL_ERR_OOM;
    }
    bool got_any = false;
    if (length) {
        csil_err e = read_full(&st->stream, buf, (size_t)length, &got_any);
        if (e || !got_any) {
            free(buf);
            return e ? e : CSIL_ERR_CARRIER;
        }
    }
    *out = buf;
    *out_len = (size_t)length;
    return CSIL_OK;
}

csil_frame_carrier csil_stream_carrier(const csil_stream *stream,
                                       size_t max_frame) {
    csil_frame_carrier c;
    memset(&c, 0, sizeof(c));
    stream_state *st = calloc(1, sizeof(*st));
    if (!st) {
        return c; /* function pointers NULL: an unusable carrier the host detects */
    }
    st->stream = *stream;
    st->max_frame = max_frame ? max_frame : CSIL_MAX_FRAME_DEFAULT;
    c.send_frame = stream_send;
    c.recv_frame = stream_recv;
    c.userdata = st;
    return c;
}

void csil_stream_carrier_dispose(csil_frame_carrier *carrier) {
    if (carrier) {
        free(carrier->userdata);
        carrier->userdata = NULL;
    }
}

/* ---- in-memory FIFO of byte frames ---------------------------------------- */

typedef struct frame_node {
    uint8_t *data;
    size_t len;
} frame_node;

typedef struct frame_queue {
    frame_node *items;
    size_t head;
    size_t count;
    size_t cap;
} frame_queue;

static csil_err queue_push(frame_queue *q, const uint8_t *data, size_t len) {
    if (q->head + q->count == q->cap) {
        if (q->head > 0) {
            memmove(q->items, q->items + q->head, q->count * sizeof(frame_node));
            q->head = 0;
        } else {
            size_t cap = q->cap ? q->cap * 2 : 8;
            frame_node *p = realloc(q->items, cap * sizeof(frame_node));
            if (!p) {
                return CSIL_ERR_OOM;
            }
            q->items = p;
            q->cap = cap;
        }
    }
    uint8_t *copy = malloc(len ? len : 1);
    if (!copy) {
        return CSIL_ERR_OOM;
    }
    if (len) {
        memcpy(copy, data, len);
    }
    q->items[q->head + q->count].data = copy;
    q->items[q->head + q->count].len = len;
    q->count++;
    return CSIL_OK;
}

static void queue_pop(frame_queue *q, uint8_t **out, size_t *out_len) {
    if (q->count == 0) {
        *out = NULL;
        *out_len = 0;
        return;
    }
    frame_node *n = &q->items[q->head];
    *out = n->data;
    *out_len = n->len;
    q->head++;
    q->count--;
}

static void queue_dispose(frame_queue *q) {
    for (size_t i = 0; i < q->count; i++) {
        free(q->items[q->head + i].data);
    }
    free(q->items);
    memset(q, 0, sizeof(*q));
}

/* ---- loopback frame carrier ----------------------------------------------- */

struct csil_loopback_frame {
    frame_queue inbound;
    frame_queue outbound;
};

csil_loopback_frame *csil_loopback_frame_new(void) {
    return calloc(1, sizeof(csil_loopback_frame));
}

void csil_loopback_frame_free(csil_loopback_frame *lb) {
    if (!lb) {
        return;
    }
    queue_dispose(&lb->inbound);
    queue_dispose(&lb->outbound);
    free(lb);
}

static csil_err loopback_frame_send(void *userdata, const uint8_t *data,
                                    size_t len) {
    csil_loopback_frame *lb = userdata;
    return queue_push(&lb->outbound, data, len);
}

static csil_err loopback_frame_recv(void *userdata, uint8_t **out,
                                    size_t *out_len) {
    csil_loopback_frame *lb = userdata;
    queue_pop(&lb->inbound, out, out_len);
    return CSIL_OK;
}

csil_frame_carrier csil_loopback_frame_carrier(csil_loopback_frame *lb) {
    csil_frame_carrier c;
    c.send_frame = loopback_frame_send;
    c.recv_frame = loopback_frame_recv;
    c.userdata = lb;
    return c;
}

csil_err csil_loopback_frame_push_inbound(csil_loopback_frame *lb,
                                          const uint8_t *data, size_t len) {
    return queue_push(&lb->inbound, data, len);
}

void csil_loopback_frame_take_outbound(csil_loopback_frame *lb, uint8_t **out,
                                       size_t *out_len) {
    queue_pop(&lb->outbound, out, out_len);
}

/* ---- loopback datagram carrier -------------------------------------------- */

struct csil_loopback_datagram {
    frame_queue inbound;
    frame_queue outbound;
};

csil_loopback_datagram *csil_loopback_datagram_new(void) {
    return calloc(1, sizeof(csil_loopback_datagram));
}

void csil_loopback_datagram_free(csil_loopback_datagram *lb) {
    if (!lb) {
        return;
    }
    queue_dispose(&lb->inbound);
    queue_dispose(&lb->outbound);
    free(lb);
}

static csil_err loopback_dgram_send(void *userdata, const uint8_t *data,
                                    size_t len) {
    csil_loopback_datagram *lb = userdata;
    return queue_push(&lb->outbound, data, len);
}

static csil_err loopback_dgram_recv(void *userdata, uint8_t **out,
                                    size_t *out_len) {
    csil_loopback_datagram *lb = userdata;
    queue_pop(&lb->inbound, out, out_len);
    return CSIL_OK;
}

csil_datagram_carrier csil_loopback_datagram_carrier(csil_loopback_datagram *lb) {
    csil_datagram_carrier c;
    c.send_datagram = loopback_dgram_send;
    c.recv_datagram = loopback_dgram_recv;
    c.userdata = lb;
    return c;
}

csil_err csil_loopback_datagram_push_inbound(csil_loopback_datagram *lb,
                                             const uint8_t *data, size_t len) {
    return queue_push(&lb->inbound, data, len);
}

void csil_loopback_datagram_take_outbound(csil_loopback_datagram *lb,
                                          uint8_t **out, size_t *out_len) {
    queue_pop(&lb->outbound, out, out_len);
}
