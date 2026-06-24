/* CSIL-RPC transport - request/response/push envelopes - see csil-rpc-transport.md.
 *
 * The envelope value types are transparent structs the host builds by hand and
 * reads by field. Optional integer fields pair a value with a has_* flag (C has
 * no Option<T>); optional text fields use NULL to mean absent. A value returned
 * by a *_decode is backed by a single arena owned by the struct: every pointer
 * inside it is valid until the matching *_free, which frees that arena once. */
#ifndef CSIL_RPC_H
#define CSIL_RPC_H

#include "csil/csil_carrier.h"
#include "csil/csil_conventions.h"

#ifdef __cplusplus
extern "C" {
#endif

/* A CSIL-RPC request (client -> server). */
typedef struct csil_rpc_request {
    const char *service; /* borrowed when host-built; arena-owned when decoded */
    const char *op;
    const uint8_t *payload; /* opaque CBOR(request type), wrapped in tag 24 on the wire */
    size_t payload_len;
    uint64_t id; /* meaningful only when has_id */
    bool has_id;
    const char *auth; /* NULL == absent */
    void *_arena;     /* opaque; non-NULL only on a decoded value */
} csil_rpc_request;

/* A CSIL-RPC response (server -> client). */
typedef struct csil_rpc_response {
    uint64_t id;
    bool has_id;
    csil_status status;
    const char *variant; /* names the declared output-choice arm; NULL == absent */
    const char *error;   /* NULL == absent */
    const uint8_t *payload; /* opaque CBOR(output type); empty when status non-zero */
    size_t payload_len;
    void *_arena;
} csil_rpc_response;

/* A CSIL-RPC server push (server -> client) for `<-` operations. */
typedef struct csil_rpc_push {
    const char *service;
    const char *event;
    const uint8_t *payload;
    size_t payload_len;
    void *_arena;
} csil_rpc_push;

/* ---- request -------------------------------------------------------------- */

/* Initialize a request with no correlation id and no per-request auth. The
 * payload bytes are borrowed; they must outlive the encode call. */
void csil_rpc_request_init(csil_rpc_request *r, const char *service, const char *op,
                           const uint8_t *payload, size_t payload_len);
csil_err csil_rpc_request_encode(const csil_rpc_request *r, uint8_t **out,
                                 size_t *out_len);
csil_err csil_rpc_request_decode(const uint8_t *frame, size_t len,
                                 csil_rpc_request *out);
void csil_rpc_request_free(csil_rpc_request *r);
bool csil_rpc_request_equal(const csil_rpc_request *a, const csil_rpc_request *b);

/* ---- response ------------------------------------------------------------- */

/* A successful (status ok) typed reply. */
void csil_rpc_response_init_ok(csil_rpc_response *r, const char *variant,
                               const uint8_t *payload, size_t payload_len);
/* A transport-level failure (no typed payload). */
void csil_rpc_response_init_transport_error(csil_rpc_response *r, csil_status status,
                                            const char *message);
csil_err csil_rpc_response_encode(const csil_rpc_response *r, uint8_t **out,
                                  size_t *out_len);
csil_err csil_rpc_response_decode(const uint8_t *frame, size_t len,
                                  csil_rpc_response *out);
void csil_rpc_response_free(csil_rpc_response *r);
bool csil_rpc_response_equal(const csil_rpc_response *a, const csil_rpc_response *b);
/* Whether a decoded response carries a non-ok transport status. The host uses
 * this after decode to surface transport failures distinctly from app errors. */
bool csil_rpc_response_is_transport_error(const csil_rpc_response *r);

/* ---- push ----------------------------------------------------------------- */

void csil_rpc_push_init(csil_rpc_push *p, const char *service, const char *event,
                        const uint8_t *payload, size_t payload_len);
csil_err csil_rpc_push_encode(const csil_rpc_push *p, uint8_t **out, size_t *out_len);
csil_err csil_rpc_push_decode(const uint8_t *frame, size_t len, csil_rpc_push *out);
void csil_rpc_push_free(csil_rpc_push *p);
bool csil_rpc_push_equal(const csil_rpc_push *a, const csil_rpc_push *b);

/* ---- client --------------------------------------------------------------- */

/* A CSIL-RPC client over a frame carrier. The carrier is injected; the client
 * owns a per-connection monotonic correlation id. One client per thread; the
 * object is not internally synchronized. */
typedef struct csil_rpc_client csil_rpc_client;

/* Create a client. multiplexed=true assigns a correlation id to every request
 * (required on WS / pipelined streams); false omits it (one-in-flight carriers
 * such as HTTP). The carrier is borrowed and must outlive the client. */
csil_rpc_client *csil_rpc_client_new(const csil_frame_carrier *carrier,
                                     bool multiplexed);
void csil_rpc_client_free(csil_rpc_client *c);

/* Invoke service/op with an encoded request payload, writing the decoded
 * response into *out (free it with csil_rpc_response_free). A non-zero transport
 * status decodes successfully and is reported via *out; the return code reflects
 * only encode/carrier/decode failures. auth may be NULL. */
csil_err csil_rpc_client_call(csil_rpc_client *c, const char *service,
                              const char *op, const uint8_t *payload,
                              size_t payload_len, const char *auth,
                              csil_rpc_response *out);

/* ---- server --------------------------------------------------------------- */

/* What a handler returns for one request: a typed reply on success, or a
 * transport status on failure. */
typedef struct csil_rpc_outcome {
    bool is_reply;
    const char *variant; /* reply arm name (is_reply) */
    const uint8_t *payload;
    size_t payload_len;
    csil_status status; /* transport status (!is_reply) */
    const char *message;
} csil_rpc_outcome;

void csil_rpc_outcome_reply(csil_rpc_outcome *o, const char *variant,
                            const uint8_t *payload, size_t payload_len);
void csil_rpc_outcome_transport(csil_rpc_outcome *o, csil_status status,
                                const char *message);

typedef struct csil_rpc_server csil_rpc_server;
csil_rpc_server *csil_rpc_server_new(const csil_frame_carrier *carrier);
void csil_rpc_server_free(csil_rpc_server *s);

/* The host handler: maps a decoded request to an outcome. */
typedef void (*csil_rpc_handler)(void *ctx, const csil_rpc_request *req,
                                 csil_rpc_outcome *out);

/* Read one request, dispatch it through handler, and write the response. Sets
 * *served=false at a clean end of stream. A malformed request is answered with a
 * malformed-envelope transport status (matching the Go/Rust references). */
csil_err csil_rpc_server_serve_one(csil_rpc_server *s, csil_rpc_handler handler,
                                   void *ctx, bool *served);

#ifdef __cplusplus
}
#endif

#endif /* CSIL_RPC_H */
