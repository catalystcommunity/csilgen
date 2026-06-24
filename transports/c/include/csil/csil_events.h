/* CSIL-Events transport - typed bidirectional event streams - see
 * csil-events-transport.md. Verbose (text-keyed) and compact (positional)
 * profiles, plus the control plane (service ordinal 0) lifecycle payloads. */
#ifndef CSIL_EVENTS_H
#define CSIL_EVENTS_H

#include "csil/csil_conventions.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Which wire profile a connection uses for its lifetime, fixed by hello. */
typedef enum csil_profile {
    CSIL_PROFILE_VERBOSE = 0,
    CSIL_PROFILE_COMPACT = 1
} csil_profile;

const char *csil_profile_name(csil_profile p);
/* Parse a wire profile name; returns true and writes *out on success. */
bool csil_profile_parse(const char *s, csil_profile *out);

/* One typed event in either direction. Identified by service+operation (verbose)
 * or by their ordinals (compact); carries an optional correlation id. Optional
 * fields use has_* flags (ordinals/id) or NULL (service/event names). */
typedef struct csil_event {
    const char *service; /* verbose name; NULL on a single-service verbose connection */
    bool has_service_ord;
    uint64_t service_ord; /* compact ordinal */
    const char *event;    /* verbose operation name */
    bool has_op_ord;
    uint64_t op_ord; /* compact ordinal */
    bool has_id;
    uint64_t id;
    const uint8_t *payload;
    size_t payload_len;
    void *_arena;
} csil_event;

/* Initialize a verbose event by name. service may be NULL. */
void csil_event_init_verbose(csil_event *e, const char *service, const char *event,
                             const uint8_t *payload, size_t payload_len);
/* Initialize a compact event by ordinals. */
void csil_event_init_compact(csil_event *e, uint64_t service_ord, uint64_t op_ord,
                             const uint8_t *payload, size_t payload_len);

csil_err csil_event_encode(const csil_event *e, csil_profile profile, uint8_t **out,
                           size_t *out_len);
csil_err csil_event_decode(const uint8_t *frame, size_t len, csil_profile profile,
                           csil_event *out);
void csil_event_free(csil_event *e);
bool csil_event_equal(const csil_event *a, const csil_event *b);

/* Control-plane operation ordinals (under service ordinal 0). */
#define CSIL_CONTROL_HELLO ((uint64_t)0)
#define CSIL_CONTROL_HELLO_ACK ((uint64_t)1)
#define CSIL_CONTROL_PING ((uint64_t)2)
#define CSIL_CONTROL_PONG ((uint64_t)3)
#define CSIL_CONTROL_CLOSE ((uint64_t)4)
#define CSIL_CONTROL_ERROR ((uint64_t)5)

/* Verbose control-event names (the `$`-sigil names). */
#define CSIL_HELLO_NAME "$hello"
#define CSIL_HELLO_ACK_NAME "$hello-ack"
#define CSIL_PING_NAME "$ping"
#define CSIL_PONG_NAME "$pong"
#define CSIL_CLOSE_NAME "$close"
#define CSIL_ERROR_NAME "$error"

/* The `$hello` payload offered by the connection initiator. The arrays are
 * borrowed when host-built and arena-owned when decoded. */
typedef struct csil_hello {
    const uint64_t *versions;
    size_t versions_len;
    const char *const *profiles;
    size_t profiles_len;
    const char *service; /* NULL == absent */
    const char *auth;    /* NULL == absent */
    void *_arena;
} csil_hello;

csil_err csil_hello_encode(const csil_hello *h, uint8_t **out, size_t *out_len);
csil_err csil_hello_decode(const uint8_t *frame, size_t len, csil_hello *out);
void csil_hello_free(csil_hello *h);

/* The `$hello-ack` payload returned by the peer. */
typedef struct csil_hello_ack {
    uint64_t v;
    const char *profile;
    const char *session; /* NULL == absent */
    void *_arena;
} csil_hello_ack;

csil_err csil_hello_ack_encode(const csil_hello_ack *a, uint8_t **out,
                               size_t *out_len);
csil_err csil_hello_ack_decode(const uint8_t *frame, size_t len,
                               csil_hello_ack *out);
void csil_hello_ack_free(csil_hello_ack *a);

/* A `$ping`/`$pong` heartbeat payload. */
typedef struct csil_heartbeat {
    uint64_t nonce;
    bool has_at;
    uint64_t at;
    void *_arena;
} csil_heartbeat;

csil_err csil_heartbeat_encode(const csil_heartbeat *h, uint8_t **out,
                               size_t *out_len);
csil_err csil_heartbeat_decode(const uint8_t *frame, size_t len,
                               csil_heartbeat *out);
void csil_heartbeat_free(csil_heartbeat *h);

/* A `$close` payload. */
typedef struct csil_close {
    csil_status status;
    const char *reason; /* NULL == absent */
    void *_arena;
} csil_close;

csil_err csil_close_encode(const csil_close *c, uint8_t **out, size_t *out_len);
csil_err csil_close_decode(const uint8_t *frame, size_t len, csil_close *out);
void csil_close_free(csil_close *c);

#ifdef __cplusplus
}
#endif

#endif /* CSIL_EVENTS_H */
