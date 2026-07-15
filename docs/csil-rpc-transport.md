# CSIL-RPC Transport

## Version 1 (Draft)

CSIL-RPC is the canonical transport for **request/response** CSIL operations
(`->`): a caller invokes `service/operation` with a typed request and receives
exactly one typed reply. It is the web-style transport — the gRPC-framing
analogue to CSIL's `.proto` analogue — and the transport that linkkeys and
longhouse speak.

This spec builds on **`csil-transport-conventions.md`** (CBOR rules, tag-24
payloads, `@wire-id`, versioning, the transport status registry, auth layers).
Read that first; only the RPC-specific parts are here.

CSIL-RPC also defines a one-way **server push** envelope for the `<-` direction
on carriers that keep a connection open, so a single RPC connection can deliver
unsolicited messages without a second transport. (Sustained bidirectional
streaming is CSIL-Events' job, not RPC's.)

---

## 1. Envelopes

CSIL-RPC has three envelopes: a request, a response, and a push. The verbose
(text-keyed map) profile is canonical for RPC; RPC does not define a compact
profile (its overhead is amortized over a full round-trip, and its carriers are
not high-frequency). All three obey the deterministic-encoding rules of the
conventions doc.

### 1.1 Request — client → server

```cddl
CsilRpcRequest = {
    v: uint,              ;; transport version, currently 1
    service: tstr,        ;; CSIL service name, e.g. "Attestation"
    op: tstr,             ;; CSIL operation name, e.g. "deposit-claim"
    ? id: uint,           ;; correlation id; REQUIRED on multiplexed carriers,
                          ;;   OPTIONAL on one-in-flight carriers
    payload: #6.24(bstr), ;; tag-24 CBOR(request type)
    ? auth: tstr          ;; per-request credential for caller-scoped ops
}
```

- **`service` / `op`** are the verbatim CSIL names. They are exactly the
  `(service, op)` pair the generated clients hand their transport seam
  (`docs/cbor-wire-contract.md`, "RPC call naming"): `service` is the CSIL
  service name as written; `op` is the CSIL operation name as written
  (kebab-case). A receiver maps `(service, op)` onto its generated router.
- **`id`** is a per-connection monotonically increasing unsigned integer. It is
  **REQUIRED** on carriers that allow more than one call in flight on one
  connection (WebSocket, a pipelined byte stream) and **OPTIONAL** on strictly
  one-in-flight carriers (HTTP, a synchronous request/response TCP exchange). When
  present in a request, the response MUST echo it.
- **`payload`** is the tag-24-wrapped CBOR encoding of the operation's request
  type, opaque to the transport.
- **`auth`** is an OPTIONAL per-request credential for operations that are
  caller-scoped (see the conventions doc, "Authentication layers"). Server↔server
  calls that authenticate by mTLS peer identity omit it.

### 1.2 Response — server → client

```cddl
CsilRpcResponse = {
    v: uint,
    ? id: uint,           ;; echoes the request id when the request carried one
    status: int,          ;; transport status (conventions doc, registry)
    ? variant: tstr,      ;; which declared output arm `payload` is; see below
    ? error: tstr,        ;; human-readable diagnostic when status != 0
    payload: #6.24(bstr)  ;; tag-24 CBOR(output type); absent/empty when status != 0
}
```

The response separates two concerns that are easy to conflate:

- **`status`** is the **transport** outcome (conventions doc registry). `0` means
  "a typed reply is present"; any non-zero value means the transport could not
  deliver a typed reply, the `payload` is empty, and `error` MAY carry a
  diagnostic string. Non-zero status is **never** how an application error is
  reported.
- **`variant`** resolves **which** declared output type the `payload` decodes to,
  when `status == 0`. A CSIL operation's output is a type choice — the success
  type plus any declared error arms:

  ```csil
  deposit-claim: DepositClaimRequest -> DepositClaimResponse / ServiceError
  ```

  Here the reply is either a `DepositClaimResponse` or a `ServiceError`, and both
  are part of the **typed contract** — the application error is not a transport
  failure. `variant` carries the **name of the chosen arm** exactly as written in
  CSIL (`"DepositClaimResponse"` or `"ServiceError"`), so the client decodes the
  payload deterministically instead of guessing by trial-decoding arms whose CBOR
  shapes might overlap.

  - When the output is a single type with no `/` arms, `variant` MAY be omitted
    and the client decodes the sole type.
  - When the output has multiple arms, `variant` is **REQUIRED** on a `status==0`
    response.

The generated client maps the reply per language idiom: the success arm becomes
the returned value; a declared error arm becomes the language's error path (Rust
`Result::Err`, Go `error`, Python raise/return, TypeScript throw); a non-zero
`status` becomes a distinct **transport** error (`ClientError::Transport` and the
like), separate from `ClientError::Service`.

### 1.3 Push — server → client (`<-` operations)

For a `<-` operation delivered over a connection-oriented RPC carrier (WebSocket,
open TCP), the server emits an unsolicited push:

```cddl
CsilRpcPush = {
    v: uint,
    service: tstr,
    event: tstr,          ;; the `<-` operation name
    payload: #6.24(bstr)  ;; tag-24 CBOR(event type)
}
```

A push has no `id` (it is not a reply to anything) and no `status` (it cannot
fail in the request/response sense). Hosts that need rich bidirectional
event flow — many event types, correlation, backpressure — SHOULD use CSIL-Events
rather than overloading RPC push.

---

## 2. Carriers

A carrier defines how an envelope is delimited on a given wire. The envelope
bytes are identical across carriers; only delimiting differs. For a worked,
per-language server that serves the same dispatch over **both** an HTTP socket and
a TCP/stream socket at once, see `serving-csil-rpc-http-and-tcp.md`.

### 2.1 HTTP

Two profiles. Both use `Content-Type: application/cbor` for request and response
bodies.

- **Envelope-in-body (default).** One `CsilRpcRequest` is the entire POST body;
  one `CsilRpcResponse` is the entire response body. The request is self-routing
  (`service`/`op` are inside it), so the **HTTP path is not semantic** — the
  canonical default mount is `POST /csil/v1/rpc`, but a host MAY mount it
  anywhere. The HTTP status SHOULD be `200` whenever a `CsilRpcResponse` is
  returned (including one carrying a non-zero transport `status`); transport-layer
  HTTP failures (e.g. `404` on a wrong mount, `413` over the size guard) are the
  carrier's own and distinct from the envelope `status`.
- **Path-routed (optional).** For REST-shaped hosts (the longhouse
  `POST /api/csil/{service}/{method}` pattern), `service` and `op` move into the
  **path** — `POST /csil/{service}/{op}` — and the body is the **payload only**
  (tag-24 CBOR of the request type), not a full envelope. The response body is
  likewise the tag-24 payload; the transport `status` is conveyed by the HTTP
  status line plus an `X-Csil-Status` header for the registry code, and `variant`
  by an `X-Csil-Variant` header. This profile trades self-description for
  REST-friendliness; the envelope-in-body profile is preferred for new hosts.
  A server MAY disable the path-routed profile by configuration (rejecting such
  requests with HTTP `404`); it is enabled by default.

`id`/`auth` map to headers in the path-routed profile (`X-Csil-Id`,
`Authorization`); in the envelope-in-body profile they live in the envelope.

### 2.2 WebSocket

Each envelope is exactly **one binary WebSocket frame** (opcode `0x2`). Text
frames are not used. The connection is multiplexed — many calls may be in flight
— so `id` is **REQUIRED** on requests and echoed on responses. Server pushes
(`CsilRpcPush`) are server→client binary frames interleaved with responses. A
credential for session auth is carried in the WebSocket handshake (a subprotocol
token or an `Authorization` header on the upgrade request); once the socket is
open it is bound to that identity.

### 2.3 Byte stream (TCP, Unix socket, TLS stream)

Envelopes are **length-prefixed**: a **4-byte big-endian unsigned length** of the
following CBOR envelope, then that many bytes.

```
+--------------------+----------------------------+
| len (u32, big-end) | CBOR envelope (len bytes)  |
+--------------------+----------------------------+
```

- The length counts only the envelope bytes, not the 4-byte prefix.
- A receiver MUST reject a length exceeding the max-frame guard (conventions doc;
  default 16 MiB) **before** allocating, and close the connection.
- The stream MAY pipeline (multiple requests before their responses); when it
  does, `id` is REQUIRED. A strictly synchronous one-in-flight stream MAY omit
  `id`.
- Session auth on a raw/TLS stream is the mTLS peer identity (server↔server) or a
  credential in the first envelope; the spec does not mandate a separate
  handshake frame for the stream carrier.

Length-prefix is canonical over reading a single self-delimited CBOR item off the
stream: it is portable to languages whose CBOR libraries do not report
bytes-consumed, and it lets a receiver enforce the size guard before decoding.

---

## 3. Worked example

Given:

```csil
@wire-id(1)
service Attestation {
    @wire-id(0)
    deposit-claim: DepositClaimRequest -> DepositClaimResponse / ServiceError
}
```

csilgen already emits (unchanged by this spec): the `DepositClaim*` and
`ServiceError` types, a typed client method
`deposit_claim(req) -> Result<DepositClaimResponse, ClientError>` that calls
`transport.call("Attestation", "deposit-claim", &req)`, and the server
handler/router. CSIL-RPC pins what `call` and the server put on the wire:

1. **Request.** The client encodes
   `CsilRpcRequest { v: 1, service: "Attestation", op: "deposit-claim",
   id: 7, payload: 24(CBOR(req)) }` and POSTs it (envelope-in-body) or sends it as
   one length-prefixed frame.
2. **Success response.** The server returns
   `CsilRpcResponse { v: 1, id: 7, status: 0, variant: "DepositClaimResponse",
   payload: 24(CBOR(resp)) }`. The client sees `status == 0`, reads
   `variant`, decodes the payload as `DepositClaimResponse`, returns `Ok(resp)`.
3. **Application error.** The server returns
   `CsilRpcResponse { v: 1, id: 7, status: 0, variant: "ServiceError",
   payload: 24(CBOR(err)) }`. `status == 0` (transport succeeded), `variant`
   selects `ServiceError`, the client surfaces it as `Err(ClientError::Service(err))`.
4. **Transport error.** The server returns
   `CsilRpcResponse { v: 1, id: 7, status: 2, error: "unknown op" }` with no
   payload. The client surfaces `Err(ClientError::Transport { status: 2, .. })`,
   distinct from any application error.

---

## 4. The generated `Transport` seam

The existing generated client transport seam is defined to produce exactly the
above. Per `docs/cbor-wire-contract.md` and `csil-spec.md`, generators emit a
`Transport`/`call(service, op, req)` shape per language; CSIL-RPC pins its
behavior:

- `call(service, op, req)` MUST encode a `CsilRpcRequest` per §1.1, deliver it via
  the chosen carrier, read a `CsilRpcResponse`, and: on `status == 0` decode and
  return the payload per `variant`; on non-zero `status` raise a transport error
  carrying the code and `error` string.

The reference libraries (`transports/<language>/`) provide
ready implementations of this seam over all three carriers (HTTP, WebSocket,
stream) plus the matched **server** side: a dispatcher that frames/deframes
envelopes and drives the generated router, returning a `CsilRpcResponse` with the
correct `status`/`variant`.

---

## 5. Conformance vectors

The following MUST be published under `transports/conformance/rpc/` and satisfied
by every implementation:

- **Request round-trip.** A fixed `(service, op, id, payload)` encodes to a
  byte-exact `CsilRpcRequest`, and those bytes decode back to the same tuple.
- **Success response.** A `status==0` response with `variant` and a payload
  decodes to the typed success value.
- **Application-error response.** A `status==0` response with
  `variant: "ServiceError"` maps to the service-error path, **not** to a decode
  of an empty success payload.
- **Transport-error response.** A non-zero `status` with `error` and no payload
  maps to a transport error, never to a (mis)decoded payload.
- **Push.** A `<-` operation produces/consumes a byte-exact `CsilRpcPush`.
- **Stream framing.** The 4-byte length prefix is byte-exact for a known
  envelope; an over-limit length is rejected without allocation.
- **Version mismatch.** An envelope with an unknown `v` is rejected with status
  `5`, not silently misparsed.
- **Cross-impl interop.** A single vector that linkkeys' Rust server and a
  generated client (any language) both satisfy.

---

## 6. Migration

- **linkkeys.** Its `RequestEnvelope { v, service, op, payload, auth }` /
  `ResponseEnvelope { v, status, error, payload }` are already the candidate for
  this spec. Migration: keep `service`/`op`/`auth`; add the optional `id`
  (required only when it multiplexes); add the `variant` discriminator to
  responses; remap its ad-hoc status ints (`2` bad payload, `3` unknown
  service/op, `4` db, `5` auth) onto the registry (`1`, `2`, `6`, `3`/`4`); switch
  its TCP carrier to the 4-byte length prefix; keep its `POST /v1alpha/rpc` HTTP
  carrier (envelope-in-body profile, optionally re-mounted at `/csil/v1/rpc`).
- **Reference to spec.** Once adopted, linkkeys' `docs/transport-and-auth.md` (and
  piler's) reference "CSIL-RPC Transport v1" instead of describing a bespoke
  envelope.
