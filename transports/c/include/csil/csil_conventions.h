/* Conventions shared by every CSIL transport - see csil-transport-conventions.md.
 *
 * This header owns the parts the three transports agree on: the error-code enum,
 * the version constant, the transport status registry, the tag-24 constant, the
 * max-frame guard, and the small ownership helpers (csil_free) that the encode
 * entry points and carriers hand results back through. */
#ifndef CSIL_CONVENTIONS_H
#define CSIL_CONVENTIONS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Every fallible function returns one of these; CSIL_OK is zero so that
 * `if (csil_..._encode(...))` reads as "if it failed". A non-OK transport
 * *status* on a decoded response is NOT one of these - that is a valid decode
 * (CSIL_OK) carrying a csil_status the host inspects separately. */
typedef enum csil_err {
    CSIL_OK = 0,
    CSIL_ERR_OOM,                  /* allocation failed */
    CSIL_ERR_MALFORMED,            /* structurally invalid envelope */
    CSIL_ERR_TRUNCATED,            /* input ended mid-item */
    CSIL_ERR_TRAILING,             /* trailing bytes after a complete envelope */
    CSIL_ERR_UNSUPPORTED_TYPE,     /* a CBOR item kind CSIL envelopes never use */
    CSIL_ERR_FRAME_TOO_LARGE,      /* frame exceeds the max-frame guard */
    CSIL_ERR_UNSUPPORTED_VERSION,  /* envelope version is not VERSION */
    CSIL_ERR_CARRIER,              /* carrier-level I/O failure */
    CSIL_ERR_INVALID_ARG           /* a NULL or otherwise invalid argument */
} csil_err;

/* CSIL_VERSION is the current transport version. A new value is minted only for a
 * breaking change to envelope layout or semantics. */
#define CSIL_VERSION ((uint64_t)1)

/* CSIL_TAG_ENCODED_CBOR is the CBOR semantic tag wrapping an embedded, opaque
 * CBOR data item (RFC 8949 sec 3.4.5.1). */
#define CSIL_TAG_ENCODED_CBOR ((uint64_t)24)

/* CSIL_CONTROL_SERVICE_ORD is the reserved service ordinal for the transport
 * control plane (Events lifecycle). */
#define CSIL_CONTROL_SERVICE_ORD ((uint64_t)0)

/* CSIL_MAX_FRAME_DEFAULT is the default max encoded envelope size for
 * stream/message carriers (16 MiB). A carrier rejects anything larger before
 * allocating for it. */
#define CSIL_MAX_FRAME_DEFAULT ((size_t)(16 * 1024 * 1024))

/* CSIL_MAX_FRAME_LIMIT is the largest max-frame limit a host may configure
 * (2 GiB - 1). The 4-byte length prefix can express more, but the limit lives in
 * a signed 32-bit integer in several supported languages, so this is the portable
 * ceiling every CSIL transport agrees on. Anything above it would also disable the
 * receive guard outright, since no wire length could exceed it. */
#define CSIL_MAX_FRAME_LIMIT ((size_t)2147483647)

/* csil_validate_max_frame reports whether max_frame is inside the conventions doc
 * range (1..CSIL_MAX_FRAME_LIMIT). 0 is not valid here: the carrier constructor
 * treats 0 as "use the default" before it validates. */
int csil_validate_max_frame(size_t max_frame);

/* CSIL_MAX_DATAGRAM_DEFAULT is the conservative max datagram size safe across
 * UDP/WebRTC/QUIC. */
#define CSIL_MAX_DATAGRAM_DEFAULT ((size_t)1200)

/* csil_status is a transport-level status, distinct from application errors
 * (which ride inside the payload as a declared error arm). Equality is by the
 * underlying code, so host-defined extension codes (>= 64) and unknown codes
 * compare correctly. The status field is the only signed integer on the wire. */
typedef struct csil_status {
    int64_t code;
} csil_status;

/* The registry codes (conventions doc sec 4). */
#define CSIL_STATUS_OK_CODE                    ((int64_t)0)
#define CSIL_STATUS_MALFORMED_ENVELOPE_CODE    ((int64_t)1)
#define CSIL_STATUS_UNKNOWN_SERVICE_OR_OP_CODE ((int64_t)2)
#define CSIL_STATUS_UNAUTHENTICATED_CODE       ((int64_t)3)
#define CSIL_STATUS_FORBIDDEN_CODE             ((int64_t)4)
#define CSIL_STATUS_VERSION_UNSUPPORTED_CODE   ((int64_t)5)
#define CSIL_STATUS_INTERNAL_CODE              ((int64_t)6)
#define CSIL_STATUS_UNAVAILABLE_CODE           ((int64_t)7)
#define CSIL_STATUS_DEADLINE_EXCEEDED_CODE     ((int64_t)8)

static inline csil_status csil_status_from_code(int64_t code) {
    csil_status s;
    s.code = code;
    return s;
}
static inline csil_status csil_status_ok(void) {
    return csil_status_from_code(CSIL_STATUS_OK_CODE);
}
static inline int64_t csil_status_code(csil_status s) { return s.code; }
static inline bool csil_status_is_ok(csil_status s) { return s.code == 0; }

/* csil_status_name returns the registry name for a status, or "other" for codes
 * outside it. The returned pointer is a static string literal; do not free it. */
const char *csil_status_name(csil_status s);

/* csil_err_str renders an error code as a static, human-readable string. */
const char *csil_err_str(csil_err e);

/* csil_free releases a byte buffer handed back by an encode entry point or a
 * carrier recv. A NULL pointer is a no-op. */
void csil_free(uint8_t *ptr);

#ifdef __cplusplus
}
#endif

#endif /* CSIL_CONVENTIONS_H */
