# CSIL-Datagrams Transport

## Version 1 (Draft)

CSIL-Datagrams is the canonical transport for **unreliable, unordered,
message-oriented** delivery of typed CSIL messages — the UDP-like sibling of
CSIL-RPC and CSIL-Events. It is for realtime media and state where *timeliness
beats completeness*: VoIP audio frames, video, game-state snapshots, telemetry,
presence pings. A lost or reordered datagram is expected and acceptable; the
transport never retransmits, never reorders, and never blocks waiting for a
missing packet.

This spec builds on **`csil-transport-conventions.md`** (CBOR rules, tag-24
payloads, `@wire-id`, versioning). Read that first. Datagrams uses the `@wire-id`
ordinals and tag-24 payload convention but, being connectionless and
overhead-sensitive, differs sharply from the other two transports — those
differences are the substance of this spec.

---

## 1. Design constraints

Three constraints shape every decision here and do not apply to RPC or Events:

1. **Each datagram is independently decodable.** Any datagram may be lost,
   duplicated, or reordered, so a datagram MUST carry everything needed to
   interpret it on its own. There is no negotiated dictionary that "arrives
   first," no cross-datagram state a decoder may assume.
2. **Overhead dominates.** A VoIP frame is ~20–60 bytes sent ~50×/s. A text-keyed
   CBOR map would multiply that. Framing overhead is measured in *bytes*, not
   fields.
3. **MTU is a hard ceiling.** A datagram MUST fit a single path packet. The
   conservative safe size across UDP/WebRTC/QUIC is **1200 bytes total**
   (envelope + payload). Messages larger than the channel's negotiated max are
   **not this transport's problem** (see Non-goals).

A datagram channel is **single-purpose**: it is bound to one CSIL service at
channel setup (out of band — the WebRTC/SDP signaling, the QUIC connection setup,
or an RPC/Events call that establishes it). Datagrams therefore do **not** carry
a service ordinal per packet (unlike Events, which multiplexes services); the
service is implied by the channel. This keeps the per-packet header minimal.

---

## 2. Profiles

A channel uses **one** profile for its lifetime, fixed at setup. Three are
defined, covering "just works," "every byte counts," and "I don't own the
framing."

### 2.1 CBOR-array (default)

The whole datagram is one deterministic CBOR array:

```cddl
CsilDatagram = [
    v: uint,              ;; transport version, currently 1
    op_ord: uint,         ;; the operation's @wire-id within the channel's service
    seq: uint,            ;; sequence number (see §3); 0 means "unsequenced"
    payload: #6.24(bstr)  ;; tag-24 CBOR(message type)
]
```

- **Fixed 4-element array**, no optional positions — positional optionals are
  error-prone, so `seq` is **always present** (a sender that does not sequence
  uses `0`). The overhead is a handful of bytes: the array header, three small
  uints, and the tag-24 prefix.
- One codec end to end, fully self-describing within the datagram. This is the
  recommended default for non-media datagram traffic — game-state snapshots,
  presence, telemetry — where a few bytes of overhead are irrelevant and a single
  CBOR path is simplest.

### 2.2 Compact fixed header (high-rate media)

For media where every byte counts, a fixed **binary** header (not CBOR) precedes
an opaque body:

```
 byte 0        byte 1        bytes 2-3       byte 4 (opt)    bytes 5.. (or 4..)
+------------+-------------+---------------+---------------+------------------+
| ver | flags| op_ord (u8) | seq (u16, BE) | epoch (u8)*   | body (opaque)    |
+------------+-------------+---------------+---------------+------------------+
  4b    4b
```

- **`ver`** (high nibble of byte 0) = `1`. **`flags`** (low nibble): bit 0 set ⇒
  an `epoch` byte is present; bits 1–3 reserved (MUST be 0).
- **`op_ord`** (byte 1) — the operation `@wire-id`, 0–255. (Channels needing more
  than 256 message types use the CBOR-array profile.)
- **`seq`** (bytes 2–3) — 16-bit big-endian sequence, RTP-style; wraps mod 2¹⁶.
- **`epoch`** (byte 4, present iff `flags` bit 0) — increments when the sender
  restarts and `seq` resets, so a receiver can tell a restart from a huge reorder.
- **`body`** — the message bytes. By default the body is the tag-24 CBOR payload;
  a channel MAY negotiate a raw body (e.g. an Opus frame) when the message type is
  itself an opaque `bytes`, avoiding even the tag-24 wrap.

This profile is for the VoIP/video case: a 4–5 byte header, no CBOR parse on the
hot path for the header, trivial to implement in a DSP/RTP-shaped pipeline.

### 2.3 Payload-only (embedded / carrier-framed)

CSIL defines **only the body** (the tag-24 CBOR of the message type, or a raw
`bytes` body); the carrier supplies type, sequence, and timing out of band. This
is the profile for stacks you don't frame yourself:

- **iOS SIP/VoIP via CallKit + PushKit.** Apple's APIs own the SIP/RTP framing;
  you only get to choose the media/body bytes. CSIL-Datagrams contributes the
  typed body and nothing else; `op_ord`/`seq` come from the RTP header the
  platform manages.
- **An existing WebRTC media track** where RTP already carries
  payload-type/sequence/timestamp.

In this profile the `@wire-id` mapping is conveyed by the channel's setup (the
SDP `a=rtpmap`/fmtp lines or the application's signaling), not in the datagram.

---

## 3. Sequencing and loss handling

CSIL-Datagrams provides the *information* to detect loss/reorder/restart and
**nothing more** — no recovery.

- **`seq`** is a per-channel counter the sender increments per datagram (per
  message type is also permissible if the channel documents it). It lets a
  receiver detect loss (gaps), reorder (out-of-order arrival), and duplication.
  `0` is the reserved "unsequenced" value for senders that don't care.
- **`epoch`** (compact profile) distinguishes a sender restart (seq reset) from a
  reorder.
- A receiver decides policy: drop late/duplicate datagrams, apply newest-wins for
  state snapshots, run a jitter buffer for media, or feed an application-level FEC
  / partial-reliability layer. The transport takes no position.

Wrap handling: the 16-bit compact `seq` wraps; receivers use standard windowed
comparison (RFC 1982 serial-number arithmetic). The CBOR-array `seq` is an
unbounded uint and need not wrap in practice.

---

## 4. Carriers

- **UDP** (native). One datagram per UDP packet. Confidentiality/integrity is the
  application's or a DTLS layer's responsibility (see Non-goals); raw UDP carries
  the datagram bytes verbatim.
- **WebRTC DataChannel, unreliable mode** (browser + native). Configured
  `{ ordered: false, maxRetransmits: 0 }`. One datagram per channel message. DTLS
  (mandatory in WebRTC) provides encryption. This is the browser/WASM path that
  works today everywhere WebRTC does.
- **QUIC / WebTransport datagrams** (RFC 9221). One datagram per QUIC DATAGRAM
  frame / WebTransport datagram. QUIC's TLS provides encryption; this is the
  cleanest modern in-browser UDP-like path (WebTransport `datagrams.writable`)
  and the preferred new-carrier for WASM clients.

The channel's profile and its service binding (and, for payload-only, the
`@wire-id`↔type map) are negotiated at carrier setup — SDP for WebRTC, the
connection-establishing RPC/Events call for QUIC/UDP — never per datagram.

---

## 5. Non-goals (explicitly out of scope for v1)

CSIL-Datagrams does **not** provide, and a conforming implementation MUST NOT
silently add:

- **Reliability / retransmission / acknowledgement.** Lost datagrams stay lost.
  Apps needing reliability use CSIL-Events or CSIL-RPC, or layer their own
  ack/FEC scheme above this transport.
- **Ordering.** Datagrams arrive in whatever order the network delivers them.
- **Fragmentation / reassembly.** A message that does not fit the channel's max
  datagram size is the application's problem. An OPTIONAL fragmentation profile
  MAY be specified later, but it is deliberately omitted from v1 because
  reassembly reintroduces the reliability/ordering concerns this transport exists
  to avoid; any such profile MUST be opt-in and clearly flagged.
- **Cryptography.** Encryption and integrity come from the carrier (DTLS for
  WebRTC, TLS for QUIC) or an application/DTLS layer over raw UDP. This spec
  defines no crypto and assumes an already-secured channel for sensitive data.
- **Per-datagram authentication.** The channel is authenticated once at setup
  (the DTLS/QUIC identity, plus the signaling channel that authorized it); there
  is no per-datagram credential — it would dwarf a media frame.

---

## 6. Worked example

```csil
@wire-id(2)
service Voice {
    @wire-id(0)
    audio: <- AudioFrame,        ;; server → client media (one-way push)
    @wire-id(1)
    mic: MicFrame ->              ;; client → server media (one-way)
}
```

A WebRTC unreliable DataChannel is negotiated for service `Voice` in the
**compact** profile during SDP signaling. Client microphone frames go out as:

```
[ 0x10 ][ 0x01 ][ seq:u16 ][ <Opus bytes> ]      ;; ver=1 flags=0, op_ord=1 (mic), raw body
```

Server audio frames arrive as:

```
[ 0x10 ][ 0x00 ][ seq:u16 ][ <Opus bytes> ]      ;; op_ord=0 (audio)
```

A 4-byte header on a 40-byte Opus frame is ~9% overhead — acceptable for media
and far below a text-keyed map. Reordered frames are dropped or jitter-buffered by
the receiver; gaps in `seq` are concealed by the audio decoder. The same `Voice`
service could simultaneously run a **CBOR-array** datagram channel for
non-media `Voice` control messages, reusing the same `op_ord`s.

For the **iOS** case, the same `Voice` service uses the **payload-only** profile:
CallKit/PushKit + the platform SIP stack own the RTP framing; CSIL contributes
only the `AudioFrame`/`MicFrame` body shape, and `op_ord`/`seq` come from RTP.

---

## 7. Conformance vectors

Published under `transports/conformance/datagrams/`:

- **CBOR-array round-trip** — a fixed `(v, op_ord, seq, payload)` encodes to a
  byte-exact `CsilDatagram` and back; a 3- or 5-element array is rejected.
- **Compact header** — a fixed `(ver, flags, op_ord, seq, epoch?, body)` encodes
  to the byte-exact header layout; the `epoch`-present and `epoch`-absent forms
  are both covered and distinguished by the flags bit.
- **Sequence/epoch** — given an out-of-order/duplicate/restart sequence of
  datagrams, the documented detection (gap, reorder, restart) is reproduced.
- **MTU guard** — a datagram exceeding the channel max is rejected by the sender
  before transmission.
- **Version drop** — a datagram with an unknown `v`/`ver` is dropped, not
  misparsed.
- **Payload-only** — a body-only datagram decodes against an externally supplied
  `op_ord`.

---

## 8. Relationship to Events

A single CSIL service can run over **both** transports at once: CSIL-Events over a
reliable channel (WebSocket/TCP) for chat, presence, and state that must arrive,
and CSIL-Datagrams over a lossy channel (WebRTC/QUIC) for voice and high-rate
position updates that must arrive *now* or not at all. Because both use the same
`@wire-id` operation ordinals, the two channels address the same operations with
the same numbers — one source of truth, two delivery guarantees.
