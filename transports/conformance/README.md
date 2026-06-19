# CSIL transport conformance vectors

These files are the **normative, language-neutral** byte-exact fixtures for the
CSIL transport family (conventions doc §8). Every reference library — and anyone
implementing a transport spec — checks its encoders and decoders against these.
When a spec and a vector disagree, that is a bug in one of them to reconcile;
neither silently wins.

- `rpc.json` — CSIL-RPC request / response / push envelopes.
- `events.json` — CSIL-Events verbose + compact event frames and control payloads.
- `datagrams.json` — CSIL-Datagrams CBOR-array and compact-header datagrams.

## Format

Each file is `{ "vectors": [ entry, ... ] }`. Each entry:

```json
{
  "name": "request_with_id",
  "description": "human-readable note",
  "input": { ...language-neutral fields... },
  "hex": "a561760162..."
}
```

- **`input`** describes the logical message in a language-neutral way (strings,
  integers, nulls, and `*_hex` fields for opaque payload/body bytes). A consumer
  reconstructs its envelope type from these fields.
- **`hex`** is the lowercase hex of the **canonically-encoded** envelope bytes.

A conforming library MUST, for every entry: build the envelope from `input`,
encode it, and get exactly `hex` (encode check); and decode `hex` back to a value
equal to that envelope (decode check). See `transports/rust/tests/conformance.rs`
for the reference consumer.

## Regenerating

The vectors are generated from the Rust reference implementation:

```
cargo run -p csilgen-transport --example gen_vectors
```

Regenerate only when intentionally changing the wire; the change then shows up as
a diff here and every language's conformance test re-verifies against it.

## `input` field reference

- **RPC** (`kind`): `request` → `service, op, id?, auth?, payload_hex`;
  `response` → `id?, status, variant?, error?, payload_hex`;
  `push` → `service, event, payload_hex`.
- **Events**: application frames carry `profile` (`verbose` → `service?, event,
  id?, payload_hex`; `compact` → `service_ord, op_ord, id?, payload_hex`); control
  payloads carry `control` (`hello`, `hello_ack`, `ping`, `close`) with their
  respective fields.
- **Datagrams** (`profile`): `cbor-array` → `op_ord, seq, payload_hex`;
  `compact-header` → `op_ord, seq, epoch?, body_hex`.
