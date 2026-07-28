# CSIL cross-language interop suite

Proves the CSIL wire standard is byte-identical and behaviorally consistent
across every generated language. One spec (`interop.csil`) is generated for each
target language; each language builds a single **harness** program that runs as
either a **server** or a **client**; then the orchestrator
(`cargo run -p xtask interop`) runs the full matrix:

```
for transport in {rpc, events, datagrams}:
  for server_lang in LANGS:
     start  harness(server_lang) server <transport> <port(server_lang)>
     for client_lang in LANGS:
        run  harness(client_lang) client <transport> <port(server_lang)>  ->  JSON results
```

Every client talks to every server (including its own) over a per-server
**loopback port** on `127.0.0.1`. Swift is excluded (no `swiftc` available).

## Transports & carriers

The matrix runs over loopback **TCP** and **UDP** (not Unix sockets) — both are in
every language's standard library, including JVM NIO, so no native/FFI socket
shims are needed in any harness.

- **CSIL-RPC** and **CSIL-Events** ride a **TCP** connection (`127.0.0.1:<port>`)
  framed with the canonical 4-byte big-endian length prefix — the transport
  library's existing `StreamCarrier` wraps the connected stream unchanged.
- **CSIL-Datagrams** ride **UDP**. The server binds `127.0.0.1:<port>`; the client
  uses an ephemeral local UDP port and the server replies to the `recvfrom`
  source. The test exercises the *envelope* round-trip, not loss semantics.

The wire envelopes and payload codecs are **carrier-independent** — the same bytes
flow over TCP/UDP here as would over Unix sockets, TLS, WebSockets, etc.

## Ports

One port per language (the server's; clients connect to it), reused across the
three transports (which run sequentially; harnesses set `SO_REUSEADDR` for clean
rebind). Base `6387`, in registry order:

| rust | go | python | typescript | java | csharp | c | ruby | elixir | dart | ocaml | zig | kotlin | swift |
|----|----|----|----|----|----|----|----|----|----|----|----|----|----|
| 6387 | 6388 | 6389 | 6390 | 6391 | 6392 | 6393 | 6394 | 6395 | 6396 | 6397 | 6398 | 6399 | 6400* |

`*` swift's port `6400` is **reserved** for when a `swiftc` toolchain is available;
swift is not in the matrix today.

## Harness CLI contract

Each language's harness is invoked as:

```
harness server <transport> <port>      # binds 127.0.0.1:<port>, prints READY, serves
harness client <transport> <port>      # connects, runs the battery, prints JSON, exits 0
```

`<transport>` is one of `rpc`, `events`, `datagrams`.

## Result protocol (client stdout)

The client prints exactly one JSON object on stdout (everything else to stderr):

```json
{
  "lang": "rust",
  "transport": "rpc",
  "cases": [
    {"name": "echo-scalars/success",        "ok": true,  "detail": ""},
    {"name": "validate-constrained/failure", "ok": true,  "detail": "got ServiceError code=422"},
    {"name": "echo-collections/success",     "ok": false, "detail": "field `color` mismatch: sent green got <null>"}
  ]
}
```

A matrix cell `(client_lang, server_lang, transport)` passes iff every case is
`ok: true`. The orchestrator records per-cell results and prints a grid plus a
non-zero exit if any case fails.

## Fixed test vectors (language-neutral)

Every client constructs these exact logical values so any server can echo them
and the client can assert structural equality. Bytes are hex; timestamps are
RFC3339 UTC; decimals are exact strings.

- **Scalars (`SCALARS_OK`)**: `i=-42, u=42, n=-7, f=3.5, t="héllo 世界", raw=0x0102f0ff, flag=true, when="2026-06-29T12:34:56Z", amount="123.45", status_literal="pending" (mixed-union literal arm, wires as [1,"pending"]), status_free="unlisted" (mixed-union general arm, wires as [0,"unlisted"]), note="info" (inline mixed-choice literal arm, hoisted to synthesized `Scalars_note`, wires as [1,"info"]), size="medium" (inline all-literal choice, wires bare, no synthesis), level="high" (named enum `Level` whose last arm is `.default`-constrained; the value used deliberately differs from the declared default "medium"), season="autumn" (named enum `Season` assembled via a base rule + `/=` extension; value comes from the extension arms), ship_text="ground" (named all-literal enum `ShipMode` with MIXED literal kinds — text + int arms — wires bare as text), ship_int=2 (same `ShipMode` type, wires bare as int)`
- **Collections (`COLLECTIONS_OK`)**: `names=["a","b"], at_least_one=[1,2,3], bounded=[10,20], exact3=[7,8,9], scores={"x":1,"y":2}, extra={"k":"v"} (any-value=text), pair=["p",5], triple=["t",9,true], color="green", prio=2, who=4242 (uint variant)`
- **Nested (`NESTED_OK`)**: `inner=SCALARS_OK, maybe=CONSTRAINED_OK, many=[SCALARS_OK]`
- **Constrained valid (`CONSTRAINED_OK`)**: `code="PRD-AB12CD", qty=10, rate="0.25", password="hunter2hunter2", tags=["one","two"]`
- **Constrained invalid (`CONSTRAINED_BAD`)**: `code="bad", qty=0, rate="9.9", password="x", tags=[]` (violates size/regex/range/length/min-items)
- **OptBytes, three states**: `{tag:"absent"}` (no `payload` key at all), `{tag:"empty", payload=0x}` (present, zero-length), `{tag:"full", payload=0x0102f0ff}` (present, four bytes). The echo must come back in the same state — absent stays absent, and present-empty must NOT arrive as absent.

## Case battery

Tiers let the orchestrator gate to the currently-passing subset while codec
gaps are being closed.

| transport  | case                          | tier | description |
|------------|-------------------------------|------|-------------|
| rpc        | echo-scalars/success          | 1    | round-trip every scalar incl. nint/timestamp/decimal |
| rpc        | echo-nested/success           | 1    | nested named records + optional present |
| rpc        | echo-collections/success      | 2    | arrays (all cardinalities), maps, any-map, tuples, enum, int-enum, union |
| rpc        | validate-constrained/success  | 2    | valid Constrained echoes (error-variant op) |
| rpc        | validate-constrained/failure  | 2    | invalid Constrained → server returns ApiError variant |
| rpc        | opt-bytes/absent              | 1    | optional `bytes` unset → key omitted, echo still unset |
| rpc        | opt-bytes/present-empty       | 1    | optional `bytes` set to empty → key present as 0x40, echo still present-and-empty |
| rpc        | opt-bytes/present-non-empty   | 1    | optional `bytes` set → key present with the bytes, echo byte-identical |
| events     | on-tick/success               | 1    | server pushes N Ticks; client verifies sequence+enum field |
| events     | duplex/success                | 1    | client sends Scalars frames; server echoes via channel router |
| events     | unknown-method/failure        | 1    | client sends an unknown channel method → server router returns error/404 |
| datagrams  | echo-scalars/success          | 1    | datagram round-trips Scalars payload (op_ord from @wire-id) |
| datagrams  | echo-collections/success      | 2    | datagram round-trips Collections payload |
| datagrams  | bad-op-ord/failure            | 1    | datagram with an unknown op_ord → server drops / signals error |

Tier 1 is byte-clean today for go/python; tier 2 depends on closing the
rust-codec enum/union/tuple/any/nint gaps and the error-variant client method
(tracked in the codec gap inventory). The bar is **all tiers green for all
language pairs**.
