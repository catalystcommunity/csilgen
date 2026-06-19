# CSIL transports

CSIL standardizes the *logical* contract (types, services, `service/operation`,
directions); csilgen generates types, typed client call sites, server
handler/router shapes, and a transport seam — but **generators emit shapes and
routing only, never the wire**. These transports fill that gap: they define how a
logical message becomes bytes and comes back, *beside* the language spec, so every
host targets one wire instead of inventing its own envelope.

There are three, one per delivery model, over a shared set of CBOR conventions:

| Transport | Model | Reliable | Ordered | Typical use |
| --------- | ----- | -------- | ------- | ----------- |
| **CSIL-RPC** | request → one response (+ push) | yes | n/a | web-style calls (linkkeys, longhouse) |
| **CSIL-Events** | persistent bidirectional typed-event stream | yes | yes | realtime apps (piler) |
| **CSIL-Datagrams** | fire-and-forget message | **no** | **no** | VoIP / media / state snapshots |

All three map onto the existing CSIL direction operators (`->`, `<->`, `<-`) and
require **no new language syntax** — except one optional, opt-in addition shared by
the compact profiles: the `@wire-id(N)` annotation that assigns stable wire
ordinals to services and operations.

## Layout

```
csil-transport-conventions.md     # shared: CBOR rules, tag-24 payloads, @wire-id,
                                  #   versioning, status registry, auth (repo root)
csil-rpc-transport.md             # CSIL-RPC spec            (repo root)
csil-events-transport.md          # CSIL-Events spec         (repo root)
csil-datagrams-transport.md       # CSIL-Datagrams spec      (repo root)
transports/
├── conformance/                  # normative byte-exact vectors (+ README)
├── rust/                         # csilgen-transport (Rust, in cargo workspace)
├── go/                           # Go reference library
├── typescript/                   # TypeScript reference library
└── python/                       # Python reference library
```

## Reference libraries

Each library is **hand-maintained** (not generated, not a wasm generator) and
provides a matched **client and server** for all three transports. They own the
envelope codecs, framing, and lifecycle; the byte/datagram **carrier is injected**
(bring-your-own-carrier), so a host plugs HTTP, WebSocket, TCP, UDP, QUIC, WebRTC,
or a platform media stack by implementing a small seam — without modifying the
library. Built-in carriers cover the common cases (length-prefixed streams, UDP,
in-memory loopback); everything else is the host's seam implementation.

The **conformance vectors** under `transports/conformance/` are the source of
truth for byte layout. They are generated from the Rust reference
(`cargo run -p csilgen-transport --example gen_vectors`) and every language's test
suite verifies its encoders/decoders against them, which is how four independent
implementations stay byte-compatible.

## Running the tests

```
cargo run -p xtask test-transports
```

runs every language's transport tests against the shared vectors, skipping any
language whose toolchain (Go, Node, Python) is not installed. Per language:

- Rust: `cargo test -p csilgen-transport`
- Go: `cd transports/go && go test ./...`
- TypeScript: `cd transports/typescript && npm test`
- Python: `cd transports/python && python3 -m unittest discover -s tests`
