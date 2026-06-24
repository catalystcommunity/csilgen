/* CSIL-Datagrams transport - unreliable, unordered, message-oriented - see
 * csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header
 * profiles. A datagram channel is single-service: the service is bound at
 * channel setup, so datagrams carry no service ordinal. */
#ifndef CSIL_DATAGRAMS_H
#define CSIL_DATAGRAMS_H

#include "csil/csil_conventions.h"

#ifdef __cplusplus
extern "C" {
#endif

/* A datagram in the CBOR-array (default) profile: [v, op_ord, seq, payload]. */
typedef struct csil_datagram {
    uint64_t op_ord;
    uint64_t seq; /* per-channel sequence; 0 means "unsequenced" */
    const uint8_t *payload;
    size_t payload_len;
    void *_arena;
} csil_datagram;

void csil_datagram_init(csil_datagram *d, uint64_t op_ord, uint64_t seq,
                        const uint8_t *payload, size_t payload_len);
csil_err csil_datagram_encode(const csil_datagram *d, uint8_t **out,
                              size_t *out_len);
csil_err csil_datagram_decode(const uint8_t *frame, size_t len,
                              csil_datagram *out);
void csil_datagram_free(csil_datagram *d);
bool csil_datagram_equal(const csil_datagram *a, const csil_datagram *b);

/* A datagram in the compact fixed-header profile. Header layout:
 * [ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8]) then the opaque body. The body
 * is borrowed when host-built and (a copy of the input) when decoded. */
typedef struct csil_compact_datagram {
    uint8_t op_ord;
    uint16_t seq;
    bool has_epoch;
    uint8_t epoch;
    const uint8_t *body;
    size_t body_len;
    uint8_t *_owned; /* heap copy backing body on a decoded value */
} csil_compact_datagram;

void csil_compact_datagram_init(csil_compact_datagram *d, uint8_t op_ord,
                                uint16_t seq, const uint8_t *body, size_t body_len);
void csil_compact_datagram_set_epoch(csil_compact_datagram *d, uint8_t epoch);
csil_err csil_compact_datagram_encode(const csil_compact_datagram *d, uint8_t **out,
                                      size_t *out_len);
csil_err csil_compact_datagram_decode(const uint8_t *frame, size_t len,
                                      csil_compact_datagram *out);
void csil_compact_datagram_free(csil_compact_datagram *d);
bool csil_compact_datagram_equal(const csil_compact_datagram *a,
                                 const csil_compact_datagram *b);

/* Sequence classification, for loss/reorder/restart detection. The transport
 * detects; the app decides. */
typedef enum csil_seq_kind {
    CSIL_SEQ_FIRST = 0,         /* first datagram seen on the channel */
    CSIL_SEQ_ADVANCED,          /* strictly newer (possibly skipping some - a gap) */
    CSIL_SEQ_LATE_OR_DUPLICATE, /* not newer */
    CSIL_SEQ_RESTART            /* sender restarted (epoch changed) */
} csil_seq_kind;

typedef struct csil_seq_event {
    csil_seq_kind kind;
    uint64_t gap; /* skipped count, meaningful only for CSIL_SEQ_ADVANCED */
} csil_seq_event;

/* Tracks the last sequence/epoch per channel to classify arrivals. Unsequenced
 * datagrams (seq 0) are reported as ADVANCED with gap 0. */
typedef struct csil_seq_tracker {
    bool has_last_seq;
    uint64_t last_seq;
    bool has_last_epoch;
    uint8_t last_epoch;
} csil_seq_tracker;

void csil_seq_tracker_init(csil_seq_tracker *t);
/* Classify an arriving (seq, epoch) and update tracker state. has_epoch=false
 * means the datagram carried no epoch. */
csil_seq_event csil_seq_tracker_observe(csil_seq_tracker *t, uint64_t seq,
                                        bool has_epoch, uint8_t epoch);

#ifdef __cplusplus
}
#endif

#endif /* CSIL_DATAGRAMS_H */
