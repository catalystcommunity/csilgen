# CSIL Transport Conventions

## Version 1 (Draft)

This document defines the conventions **shared** by every CSIL transport —
**CSIL-RPC**, **CSIL-Events**, and **CSIL-Datagrams**. Each of those specs is a
sibling document that references this one for the common parts (CBOR rules,
payload framing, the `@wire-id` ordinal system, version negotiation, and the
transport status registry) and adds only what is specific to its delivery model.

CSIL itself describes the *logical* contract — types, services,
`service/operation`, request/response, and direction. csilgen generates the
in-memory types, typed client call sites, server handler interfaces + routers,
and a transport seam. By deliberate design, **generators emit shapes and routing
only — never the wire** (`csil-spec.md`, "What generators emit for each
direction"). These transport specs fill the one thing left undefined: **the
envelope** — how a logical message becomes bytes and how it comes back — without
pushing wire code into the generators.

The normative artifacts of the transport layer are:

1. **These spec documents** (this file plus the three transport specs).
2. **Conformance vectors** — byte-exact fixtures, checked into the repo, that any
   implementation self-checks against. The vectors, not any single library, are
   the source of truth for byte layout.
3. **Reference libraries** (`transports/{rust,go,typescript,python}/`) —
   hand-maintained, *not* generated, providing matched client and server ends for
   all three transports. A host wires generated shapes onto a reference transport
   and never touches the wire.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHOULD**,
**SHOULD NOT**, **MAY**, and **OPTIONAL** are to be interpreted as in RFC 2119 /
RFC 8174.

---

## 1. CBOR rules

All CSIL transports use **CBOR** (RFC 8949). The following rules apply to every
envelope across every transport.

- **Encoding determinism.** Envelopes MUST be encoded using CBOR
  [Core Deterministic Encoding](https://www.rfc-editor.org/rfc/rfc8949#section-4.2.1):
  shortest-form integers and lengths, definite-length items only, and — for
  map-shaped envelopes — keys sorted in bytewise lexicographic order of their
  encodings. Determinism is what makes the conformance vectors byte-exact and
  lets two independently-generated parties agree without negotiation.
- **No indefinite-length items** anywhere in an envelope. (A *payload* is opaque;
  see below.)
- **Payloads are opaque and self-contained.** The bytes produced by encoding an
  operation's CSIL request/response/event type are treated by the transport as an
  opaque blob. They MUST be carried wrapped in **CBOR tag 24**
  (`encoded-cbor`, RFC 8949 §3.4.5.1): a byte string whose content is itself a
  single encoded CBOR data item. Tag 24 makes the embedded item self-announcing —
  generic tooling can recognize that the byte string is CBOR — at zero wire cost
  beyond the one-byte tag. The transport MUST NOT inspect, re-key, or re-encode
  payload bytes.

  ```cddl
  payload = #6.24(bstr)   ;; tag 24 wrapping CBOR(<the CSIL type>)
  ```

- **Field keys in payloads** follow `docs/cbor-wire-contract.md` unchanged: a
  record encodes as a CBOR map keyed by the **CSIL field names verbatim**
  (snake_case). The transport envelope keys defined in these specs are a
  *separate namespace* from payload field keys; they never collide.

### Envelope key style

Envelopes come in two profiles wherever a transport offers them:

- **Verbose** — a CBOR **map with text keys** (`"v"`, `"service"`, `"op"`, …).
  Self-describing and debuggable. This is the default everywhere.
- **Compact** — a CBOR **array** with positional fields, no keys. Used on
  high-rate carriers where per-frame key overhead matters. Position assignments
  are fixed by each transport spec.

A given connection/channel uses one profile for its lifetime; the profile is
fixed by negotiation (Events) or by channel setup (Datagrams), never mixed
frame-to-frame.

---

## 2. Versioning

Every transport carries a single unsigned integer version, `v`, starting at
**1**. A new `v` is minted only for a **breaking** change to envelope layout or
semantics; additive, backward-compatible changes (a new optional map field, a new
status code) do **not** bump `v`.

- **RPC** and **Datagrams** are stateless per message: `v` appears in every
  envelope/datagram. A receiver that does not support a given `v` MUST reject the
  message with transport status `5` (version-unsupported) for RPC, or silently
  drop it for Datagrams (a connectionless carrier has nowhere to reply).
- **Events** negotiates once: the `v` (and the profile) are agreed in the
  `hello`/`hello-ack` control exchange (see the Events spec). A mismatch closes
  the connection with status `5`.

Implementations MUST NOT silently misparse a message of an unknown version as if
it were a known one.

---

## 3. The `@wire-id` ordinal system

The verbose profiles address services and operations by their **text names**
(the CSIL service name and operation name). The compact profiles address them by
small unsigned integer **ordinals** instead, to avoid re-spelling names on every
frame. Ordinals are assigned in the CSIL source with the `@wire-id` annotation so
that two independently-generated parties derive identical numbers from one source
of truth.

### Syntax

`@wire-id(N)` is a leading annotation (its own line, like `;;;` doc comments and
field metadata) attaching to a **service declaration** and to an **operation**:

```csil
;;; The attestation service.
@wire-id(1)
service Attestation {
    ;;; Deposit a claim.
    @wire-id(0)
    deposit-claim: DepositClaimRequest -> DepositClaimResponse / ServiceError,

    @wire-id(1)
    revoke-claim: RevokeClaimRequest -> RevokeClaimResponse / ServiceError
}
```

- The **service** ordinal namespaces its operations.
- An **operation** ordinal is unique **within its service**.

### Rules (validated at generation time — the validate-early idiom)

1. **All-or-nothing.** Within a single CSIL compilation, either every service and
   every operation carries a `@wire-id`, or none do. A partial set is a hard
   error. (A spec that never uses a compact profile simply omits them all.)
2. **Uniqueness.** Service ordinals are unique across the compilation; operation
   ordinals are unique within their service.
3. **Range.** Ordinals are unsigned. **Service ordinal `0` is reserved** for the
   transport's control plane (see below); application services MUST use `≥ 1`.
   Operation ordinals start at `0` within each service.
4. **Stability.** An assigned `@wire-id` is part of the wire contract. Changing or
   removing one, or adding the system to a service that lacked it, is a
   **breaking change** and is reported as such by `csilgen breaking`. Treat
   ordinals like protobuf field numbers: append, never renumber.

A compact profile **requires** that the spec carry `@wire-id`s; a reference
transport constructed for a compact profile against a spec without them fails at
construction with a clear error, never at runtime mid-stream.

### The control plane is service ordinal `0`

Transport lifecycle messages (handshake, heartbeat, close, transport-level
errors) are modeled as operations of a reserved pseudo-service with **service
ordinal `0`**. Their operation ordinals are assigned by each transport spec.
Because control lives under service `0`, application operation ordinals are free
to start at `0` within their own service without colliding. In the verbose
profile, control messages use reserved operation names prefixed with `$` (e.g.
`$hello`), which cannot collide with CSIL operation names (those are kebab-case
identifiers that never begin with `$`).

---

## 4. Transport status registry

Transports that carry a reply (RPC, and Events' correlated replies) use a single
signed-integer **transport status**. It describes the fate of the *transport
exchange*, and is deliberately **distinct from application errors**, which are
part of the typed contract (an operation's declared `/ ErrorType` arms) and ride
inside the payload — see each spec's "variant" handling.

| Status | Name | Meaning |
| -----: | ---- | ------- |
| `0` | ok | A typed reply is present; decode the payload per the `variant`. |
| `1` | malformed-envelope | The envelope did not parse or violated these conventions. |
| `2` | unknown-service-or-op | No such service/operation (or no such ordinal). |
| `3` | unauthenticated | No valid session/credential established. |
| `4` | forbidden | Authenticated, but not permitted to call this operation. |
| `5` | version-unsupported | The envelope's `v` is not supported by the receiver. |
| `6` | internal | Receiver-side failure not attributable to the request. |
| `7` | unavailable | Temporarily overloaded / backpressured; retry may succeed. |
| `8` | deadline-exceeded | The receiver gave up before producing a reply. |

- Codes **`0`–`63` are reserved** for this registry; future minor versions may
  add codes in this range without bumping `v`.
- Codes **`≥ 64`** are available for host-specific transport extensions. A host
  using them SHOULD document its meanings; a generic client treats any unknown
  non-zero code as a transport failure.
- A non-zero status means **no typed payload is present** (the payload is empty);
  the human-readable `error` string, when present, is for diagnostics only and
  MUST NOT be parsed for control flow.

---

## 5. Framing and size limits

Each transport spec defines how envelopes are delimited on its carriers
(length-prefix on byte streams, one frame per WebSocket message, one datagram per
UDP/WebRTC/QUIC datagram, one body per HTTP message). Two rules are shared:

- **Max frame guard.** Every stream/message carrier MUST enforce a configurable
  maximum encoded envelope size and reject anything larger before allocating for
  it, so a hostile or corrupt length prefix cannot exhaust memory. The default is
  **16 MiB**; hosts may lower it. Datagrams have their own, much smaller, MTU
  limit (see that spec).
- **Self-delimited payloads.** Because the payload is a tag-24 byte string, its
  length is known from the envelope; a receiver never needs to guess where the
  payload ends.

---

## 6. Authentication layers

Two layers exist; each transport spec states how its carriers convey them.

- **Session / transport auth** — established once for a connection (or per HTTP
  request): mutual-TLS peer identity for server-to-server byte streams, a
  credential carried in the Events `hello` or in an HTTP `Authorization` header,
  or the DTLS/QUIC identity of a datagram channel. Once established, the session
  is bound to an identity and the receiver authorizes every message against it,
  **never** trusting a client-asserted identity.
- **Per-request auth** — an OPTIONAL credential attached to an individual
  operation for caller-scoped calls (RPC's `auth` field). Not all transports
  expose this (Datagrams does not — per-datagram credentials are too costly).

Transport-level confidentiality and integrity are provided by the carrier (TLS,
DTLS, QUIC). These specs do **not** define their own cryptography.

---

## 7. Reference library architecture: bring-your-own-carrier

The reference libraries own everything *above* the wire bytes — envelope
encode/decode, the `@wire-id` ↔ type ↔ operation mapping, framing (length
prefix, frame boundaries), the connection lifecycle, correlation, and the
status/variant logic. They do **not** own the byte transport itself. The carrier
is an **injected dependency** behind a small seam, so a host can supply an exotic
or proprietary carrier (QUIC, WebRTC with full ICE/DTLS, a platform RTP stack, an
in-process channel) **without modifying the library**.

Each language exposes the seam idiomatically, but the shape is the same in all
four:

- **Stream/frame transports** (RPC, Events) take a carrier that can *send a
  delimited message* and *receive a delimited message* (`send(bytes)` /
  `recv() -> bytes`, plus open/close). The library handles length-prefixing,
  frame boundaries, and lifecycle on top. Built-in implementations cover HTTP,
  WebSocket, and TCP/TLS streams; a host implements the same seam for anything
  else.
- **Datagram transports** take a carrier that can *send one datagram* and
  *receive one datagram* (each ≤ the channel MTU), with no delivery or ordering
  guarantee. The library handles the datagram envelope/header, sequence accounting
  hooks, and profile selection. Built-in implementations cover UDP and (in the
  browser/TS lib) a WebRTC unreliable DataChannel; a host plugs QUIC datagrams,
  WebTransport, or a platform media stack into the same seam.

The conformance vectors test the library *above* the seam (envelope bytes,
framing, lifecycle) independently of any real socket, which is why a library can
be fully vector-tested even for carriers it does not ship a built-in for.

## 8. Conformance vectors

Each transport spec enumerates the vectors it requires. A vector is a named tuple
of `(inputs, exact bytes)` such that:

- encoding the inputs MUST produce the exact bytes, and
- decoding the exact bytes MUST reproduce the inputs (or the defined rejection).

Vectors live under `transports/conformance/` as CBOR-diagnostic + hex pairs and
are generated once from the Rust reference implementation, then consumed
unchanged by every language's reference library and by anyone implementing the
spec. When a spec and the vectors disagree, that is a bug in one of them to be
reconciled — neither silently wins.
