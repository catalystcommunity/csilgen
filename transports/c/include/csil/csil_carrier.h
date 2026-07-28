/* Carrier seams - the bring-your-own-carrier boundary (conventions doc sec 7).
 *
 * The library owns envelope codecs, framing, and lifecycle; the *carrier* (the
 * byte/datagram transport) is injected as a vtable of function pointers plus an
 * opaque userdata the host threads through. A host supplying QUIC/WebRTC/a media
 * stack fills in two function pointers and a userdata pointer to its own
 * connection object - no library edit. All calls are synchronous and blocking;
 * the host owns the I/O loop and threads. */
#ifndef CSIL_CARRIER_H
#define CSIL_CARRIER_H

#include "csil/csil_conventions.h"

#ifdef __cplusplus
extern "C" {
#endif

/* A frame carrier sends and receives one delimited message at a time (CSIL-RPC
 * and CSIL-Events). Built-in implementations frame with a 4-byte big-endian
 * length prefix; a host may implement this over WebSocket binary frames, a
 * WebTransport stream, etc. */
typedef struct csil_frame_carrier {
    /* Send one delimited message. Returns CSIL_OK or a csil_err. */
    csil_err (*send_frame)(void *userdata, const uint8_t *data, size_t len);
    /* Receive the next frame. On a clean end of stream set *out=NULL,*out_len=0
     * and return CSIL_OK. Ownership of *out transfers to the caller, freed via
     * csil_free. */
    csil_err (*recv_frame)(void *userdata, uint8_t **out, size_t *out_len);
    void *userdata;
} csil_frame_carrier;

/* A datagram carrier sends and receives one self-contained datagram (each within
 * the channel MTU), with no delivery or ordering guarantee (CSIL-Datagrams). */
typedef struct csil_datagram_carrier {
    csil_err (*send_datagram)(void *userdata, const uint8_t *data, size_t len);
    /* Receive the next datagram. On a closed carrier set *out=NULL,*out_len=0
     * and return CSIL_OK. Ownership of *out transfers to the caller (csil_free). */
    csil_err (*recv_datagram)(void *userdata, uint8_t **out, size_t *out_len);
    void *userdata;
} csil_datagram_carrier;

/* ---- length-prefixed stream framing --------------------------------------- */

/* A read/write byte-stream seam (TCP, TLS, a Unix socket) the built-in
 * length-prefix carrier frames over. read returns the number of bytes read (0 at
 * a clean EOF) or a negative value on error; write returns 0 on success. */
typedef struct csil_stream {
    /* Read up to len bytes; return bytes read, 0 at EOF, or <0 on error. */
    long (*read)(void *userdata, uint8_t *buf, size_t len);
    /* Write exactly len bytes; return 0 on success or <0 on error. */
    int (*write)(void *userdata, const uint8_t *buf, size_t len);
    void *userdata;
} csil_stream;

/* Build a frame carrier over a byte stream using the canonical 4-byte
 * big-endian length-prefix framing, enforcing max_frame (0 selects
 * CSIL_MAX_FRAME_DEFAULT) before allocating or sending. The returned carrier's
 * userdata points at a heap copy of *stream; release it with
 * csil_stream_carrier_dispose.
 *
 * A max_frame outside 1..CSIL_MAX_FRAME_LIMIT yields a carrier with NULL
 * send_frame/recv_frame — the same unusable-carrier signal as an allocation
 * failure, so the host detects a misconfigured limit at construction. */
csil_frame_carrier csil_stream_carrier(const csil_stream *stream, size_t max_frame);
void csil_stream_carrier_dispose(csil_frame_carrier *carrier);

/* ---- in-memory loopback carriers (tests, codec drives) -------------------- */

/* An in-memory frame carrier backed by FIFO queues of frames. push_inbound
 * queues a frame a later recv_frame returns; take_outbound takes the next frame
 * a send_frame produced. */
typedef struct csil_loopback_frame csil_loopback_frame;
csil_loopback_frame *csil_loopback_frame_new(void);
void csil_loopback_frame_free(csil_loopback_frame *lb);
csil_frame_carrier csil_loopback_frame_carrier(csil_loopback_frame *lb);
csil_err csil_loopback_frame_push_inbound(csil_loopback_frame *lb,
                                          const uint8_t *data, size_t len);
/* Take the next outbound frame (caller frees with csil_free); *out=NULL if none. */
void csil_loopback_frame_take_outbound(csil_loopback_frame *lb, uint8_t **out,
                                       size_t *out_len);

typedef struct csil_loopback_datagram csil_loopback_datagram;
csil_loopback_datagram *csil_loopback_datagram_new(void);
void csil_loopback_datagram_free(csil_loopback_datagram *lb);
csil_datagram_carrier csil_loopback_datagram_carrier(csil_loopback_datagram *lb);
csil_err csil_loopback_datagram_push_inbound(csil_loopback_datagram *lb,
                                             const uint8_t *data, size_t len);
void csil_loopback_datagram_take_outbound(csil_loopback_datagram *lb,
                                          uint8_t **out, size_t *out_len);

#ifdef __cplusplus
}
#endif

#endif /* CSIL_CARRIER_H */
