# CSIL-Events Transport

## Version 1 (Draft)

CSIL-Events is the canonical transport for **realtime, bidirectional, typed
message streams** over a single persistent connection — the `<->` and `<-`
directions kept open for the life of the connection. It is CSIL-RPC's
persistent-connection sibling: same carriers and the same CBOR conventions, but
the unit on the wire is a **typed event**, not a correlated request/response.
Either side MAY send any of its declared event types at any time. This is the
transport for apps like piler — a tiled MMO sending movement, chat, presence, and
room-state deltas continuously in both directions.

This spec builds on **`csil-transport-conventions.md`** (CBOR rules, tag-24
payloads, `@wire-id`, versioning, status registry, auth). Read that first.

Events deliberately has **no per-frame `status`**: a stream of events is not a
sequence of replies. Failures are either themselves events (an application error
type flowing as an event) or, for transport-level problems, control-plane events
(§3). Request/reply *over* the stream is available when wanted, via the optional
correlation `id` (§2), but it is not the default framing.

---

## 1. Relationship to RPC and to operations

| | CSIL-RPC | CSIL-Events |
| --- | --- | --- |
| Unit | request → one response | one typed event, either direction |
| Correlation | always (1 req ↔ 1 resp) | optional (`id`, only for request/reply over the stream) |
| Per-message status | yes | no (control plane handles transport errors) |
| Lifetime | per call | per connection |
| CSIL directions | `->` | `<->`, `<-` |

An operation `sub: Topic <-> Update` declares two event types flowing on the
connection: the initiating side sends `Topic`, the other sends `Update`. A `<-`
operation (`notify: <- Event`) is one-way server→client. The **direction of an
event is implied by which side sent it**; the operation's `@wire-id` identifies
*which* operation, and a type-choice (`/=`) union inside the declared type is
resolved by the decoder. Generators already emit the inbound router and the
outbound `(op, bytes)` encoders for these directions
(`csil-spec.md`); CSIL-Events pins the envelope those bytes ride in and adds the
connection lifecycle.

---

## 2. Event envelope — two profiles

A connection uses **one** profile for its lifetime, fixed by the `hello`
handshake (§3). The verbose profile is the default; the compact profile is opted
into for high-rate connections.

### 2.1 Verbose profile (default, text-keyed)

```cddl
CsilEvent = {
    ? service: tstr,      ;; CSIL service name; MAY be omitted on a
                          ;;   single-service connection (implied by hello)
    event: tstr,          ;; the <-> / <- operation name
    payload: #6.24(bstr), ;; tag-24 CBOR(event type)
    ? id: uint            ;; correlation id; present only when this event is a
                          ;;   request expecting a reply, or is that reply
}
```

- **`service`** is omitted when the connection's `hello` bound it to a single
  service (the common realtime case); included when the connection multiplexes
  several services.
- **`id`** is OPTIONAL and used only for **request/reply over the stream**: a
  sender assigns an `id` to an event that expects a reply; the replying side
  echoes the same `id` on its response event. Fire-and-forget events carry no
  `id`. (Events does not impose RPC's status/variant on these; the reply is just
  another typed event with a matching `id`.)

### 2.2 Compact profile (positional array)

```cddl
;; fire-and-forget event
CsilEventCompact      = [service_ord: uint, op_ord: uint, payload: #6.24(bstr)]
;; correlated event (request or reply)
CsilEventCompactCorr  = [service_ord: uint, op_ord: uint, id: uint, payload: #6.24(bstr)]
```

- `service_ord` / `op_ord` are the `@wire-id` ordinals (conventions doc, §3).
  Both are always present — even a single-service connection carries the service
  ordinal, so control-plane frames (service ord `0`) and application frames share
  one uniform shape. The 1-byte cost of a small ordinal is negligible.
- A receiver distinguishes the two array shapes by **length** (3 vs 4 elements):
  a 4-element array carries a correlation `id` as the third element.
- The compact profile **requires** the spec to carry `@wire-id`s; a connection
  negotiated as compact against a spec without them is a construction-time error.

The compact profile is what makes Events viable for a game loop: a movement event
is `[service_ord, op_ord, 24(payload)]` — a few bytes of framing around the
payload, no text keys re-spelled dozens of times per second.

---

## 3. Connection lifecycle — the control plane (service ordinal 0)

Lifecycle messages are operations of the reserved control pseudo-service,
**service ordinal `0`** (conventions doc, §3). In the verbose profile they appear
as events with reserved `$`-prefixed names and no `service`; in the compact
profile as arrays with `service_ord == 0`. Their operation ordinals:

| op_ord | verbose name | direction | purpose |
| -----: | ------------ | --------- | ------- |
| `0` | `$hello` | initiator → peer | open: offered versions, profile, capabilities, optional auth, bound service |
| `1` | `$hello-ack` | peer → initiator | accept: chosen version + profile, assigned session, capabilities |
| `2` | `$ping` | either | heartbeat; carries a nonce/timestamp |
| `3` | `$pong` | either | heartbeat reply; echoes the `$ping` nonce (RTT) |
| `4` | `$close` | either | orderly shutdown: a transport status code + reason |
| `5` | `$error` | either | transport-level error not tied to a correlated event |

The control payloads are themselves CBOR maps (carried in the same `payload`
slot):

```cddl
Hello = {
    versions: [+ uint],     ;; transport versions the sender supports, preferred first
    profiles: [+ tstr],     ;; "verbose" and/or "compact", preferred first
    ? service: tstr,        ;; bind the connection to a single service (omit for multi-service)
    ? auth: tstr,           ;; session credential
    ? caps: { * tstr => any } ;; optional capability hints (heartbeat interval, max-frame, …)
}
HelloAck = {
    v: uint,                ;; chosen transport version
    profile: tstr,          ;; chosen profile ("verbose" | "compact")
    ? session: tstr,        ;; opaque session handle the peer assigns
    ? caps: { * tstr => any }
}
Ping  = { nonce: uint, ? at: uint }   ;; at = sender clock (ms), optional
Pong  = { nonce: uint, ? at: uint }
Close = { status: int, ? reason: tstr } ;; status from the transport registry
Error = { status: int, ? reason: tstr, ? id: uint } ;; id: the event this concerns, if any
```

Lifecycle rules:

1. **Handshake.** The initiator (the side that opened the carrier — the client for
   WS/TCP) sends `$hello` first. The peer replies `$hello-ack` selecting one `v`
   and one `profile` from the offered lists, or sends `$close` with status `5`
   (version-unsupported) if it can satisfy neither. No application events flow
   before `$hello-ack`. The chosen profile governs all subsequent frames
   (including later control frames).
2. **Auth.** If the operations require a session identity, the credential is in
   `Hello.auth` (or, for mTLS streams, the peer identity); once `$hello-ack`
   succeeds the connection is bound to that identity and every event is authorized
   against it. An unauthenticated/forbidden hello is answered with `$close`
   status `3`/`4`.
3. **Heartbeat.** Either side MAY send `$ping`; the receiver MUST answer `$pong`
   echoing the nonce. The negotiated `caps` MAY carry a heartbeat interval; a peer
   that misses heartbeats MAY `$close` with status `8` (deadline-exceeded).
4. **Close.** Either side sends `$close` and then stops sending application events;
   the carrier is closed after the `$close` is flushed. A carrier that drops
   without a `$close` is an abnormal close — the peer treats in-flight correlated
   events as failed.
5. **Error.** `$error` reports a transport-level problem (e.g. an undecodable
   frame, an unknown ordinal) that is not the reply to a specific correlated
   event; if it concerns a specific correlated `id`, it carries that `id`.

---

## 4. Carriers

- **WebSocket** (browsers, WASM clients). Each event/control frame is one
  **binary** WS frame (opcode `0x2`). The `$hello` credential MAY instead ride the
  WS upgrade handshake (subprotocol token / `Authorization`); the spec accepts
  either. This is piler's primary carrier.
- **Byte stream (TCP / TLS / Unix socket)** (native clients, server↔server).
  Each frame is **length-prefixed** identically to CSIL-RPC's stream carrier
  (4-byte big-endian length + CBOR), with the same max-frame guard. The `$hello`
  is the first frame.
- **WebTransport streams** (optional, modern browsers). A bidirectional
  WebTransport stream carries the same length-prefixed framing as the byte-stream
  carrier. (WebTransport *datagrams* are CSIL-Datagrams, not Events.)

Flow control in v1 is the carrier's: TCP's window, the WebSocket implementation's
buffering, WebTransport's per-stream flow control. Events does not add a
credit-based scheme in v1; a sender that must not overrun a slow receiver relies
on the carrier's backpressure (a full send buffer) and MAY use `$ping`/`$pong`
RTT as a liveness signal. Application-level flow control (credits, max in-flight)
is left to future versions and noted as such.

---

## 5. Worked example (piler)

```csil
@wire-id(1)
service World {
    @wire-id(0)
    move: MoveIntent <- RoomDelta,          ;; client sends MoveIntent, server pushes RoomDelta
    @wire-id(1)
    chat: ChatMessage <-> ChatMessage,      ;; both directions
    @wire-id(2)
    room-state: RoomQuery <-> RoomState     ;; request/reply over the stream
}
```

On a **compact, single-service** connection bound to `World` (service ord 1):

- Client opens WS, sends `$hello`:
  `[0, 0, 24(Hello{ versions:[1], profiles:["compact","verbose"], service:"World", auth:"…" })]`.
- Server replies `$hello-ack`:
  `[0, 1, 24(HelloAck{ v:1, profile:"compact", session:"…" })]`.
- Client sends a movement intent (fire-and-forget):
  `[1, 0, 24(CBOR(MoveIntent))]`.
- Server pushes a room delta (fire-and-forget, same op, opposite direction):
  `[1, 0, 24(CBOR(RoomDelta))]`.
- Client requests room state with correlation id 42:
  `[1, 2, 42, 24(CBOR(RoomQuery))]`; server replies `[1, 2, 42, 24(CBOR(RoomState))]`.
- Either side heartbeats: `[0, 2, 24(Ping{nonce:9})]` → `[0, 3, 24(Pong{nonce:9})]`.

piler's existing `ClientMessage { kind, body }` / `ServerMessage { event, body }`
collapse directly onto `CsilEvent` (or its compact form); `kind`/`event` become
the operation, `body` becomes the tag-24 `payload`.

---

## 6. Conformance vectors

Published under `transports/conformance/events/`:

- **Verbose round-trip** — a `(service?, event, id?, payload)` encodes to a
  byte-exact `CsilEvent` and back.
- **Compact round-trip** — the 3-element and 4-element arrays are byte-exact and
  distinguished by length on decode.
- **Handshake** — a `$hello`/`$hello-ack` pair selecting `compact` is byte-exact;
  a `$hello` offering only an unsupported version yields a `$close` status `5`.
- **Heartbeat** — `$ping`/`$pong` nonce echo round-trips.
- **Correlation** — a request event and its reply share an `id`; a reply with no
  matching outstanding `id` is surfaced as an `$error`.
- **Profile lock** — a frame in the wrong profile for the negotiated connection is
  rejected (not silently coerced).
- **Single vs multi-service** — service omitted (verbose) / service ord present
  (compact) both route correctly.

---

## 7. Migration

- **piler.** Replace `{kind, body}`/`{event, body}` with `CsilEvent` (verbose to
  start, compact once `@wire-id`s are assigned). Add the `$hello`/`$hello-ack`
  handshake to its WS bridge; bind the authenticated session to the character
  identity as it does today. Because piler authenticates against linkkeys and
  both then speak the CSIL transport family, the two share one wire and one auth
  story.
