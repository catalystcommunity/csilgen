# CSIL cross-language interop suite

Proves the CSIL wire standard is byte-identical and behaviorally consistent
across every generated language. One spec (`interop.csil`) is generated for each
target language. Each language builds one **harness** program that runs as a
**server** or a **client**. The trusted Python orchestrator then runs the full
matrix. For local development, `cargo run -p xtask interop` calls the same
Python implementation. Use `--servers` to run one server-language shard:

```bash
cargo run -p xtask interop --servers rust,go,python,typescript
```

```
for transport in {rpc, events, datagrams}:
  for server_lang in LANGS:
     start  harness(server_lang) server <transport> <port(server_lang)>
     for client_lang in LANGS:
        run  harness(client_lang) client <transport> <port(server_lang)>  ->  JSON results
```

Every selected client talks to every selected server, including itself when
it is in both sets. The tests use one loopback port for each server on
`127.0.0.1`. All 14 code-generating languages are in the matrix. PHP has no
harness, so it runs only the fixtures.

## Typed RPC replies

Build the CLI and the WASM generators in release mode. Run the Rust and Go
test from the repository root. Each peer writes reply envelopes and reads
the other peer's bytes. The test checks success, a declared error, transport
status 2, missing and unknown variants, and a malformed payload. The
refusal uses the named member `application_user_limit`. The test checks
the assigned CBOR integer `7` against the expected payload bytes.

```bash
tests/interop/typed-replies/run.sh
```

Run the same operation with the other language targets. Each test needs
the compiler or runtime for its target. The JSON and OpenAPI tests check
the schema references for multiple error arms and for a named success union.
The TypeScript check needs Node 22.18 or later and TypeScript 5.8 or later.
It imports each target directly in Node and runs the typed-replies peer
from both TypeScript source and compiled JavaScript.

```bash
for target in c csharp dart elixir java kotlin ocaml php python ruby swift typescript zig json openapi; do
    tests/interop/typed-replies/check.sh "$target" || exit 1
done
```

## Repository specs

This check generates every valid `.csil` file in the repository for one
target. Then it compiles the output with the strict checks of that target. The
check skips a file that `csilgen validate` refuses, because that file is a
negative fixture. Each of the 15 targets has a strict check, with warnings as
errors: Rust `rustfmt --check` and `cargo clippy -D warnings`, Swift `swiftc
-typecheck`, C `cc -Werror`, Zig `zig test` over every declaration, Go `go vet`,
TypeScript strict `tsc`, Python import and a round trip of each record, Java
`javac -Werror`, Kotlin Gradle with `allWarningsAsErrors`, C# `dotnet build
-warnaserror`, OCaml `dune build`, Elixir `elixirc --warnings-as-errors`, Dart
`dart analyze --fatal-infos`, Ruby `ruby -W` load, and PHP load with every notice
as an error. `dotted-enum-values.csil` in this directory is a spec for these
checks.

```bash
tests/interop/repo-specs/check.sh rust
tests/interop/repo-specs/check.sh kotlin
```

A new fixture spec is part of this check automatically. When a generator
writes code that does not compile for a valid spec, this check fails.

## Wire vectors

`wire-vectors/vectors.tsv` holds CBOR inputs for the shapes in `wire.csil`. Each
language's peer decodes every row as the type that its kind names. A row marked
`err` must fail to decode. A row marked `ok` must re-encode to exactly the
canonical bytes. The rows check dynamic map key order and duplicate keys, record
fields in any order, a record union made with `//=` across an include (its arm
indexes follow declaration order), and an operation output with an inline arm.
`make-vectors.py` writes the file and explains each row.

```bash
tests/interop/wire-vectors/check.sh go
```

## Formatting

This check generates `formatting/stress.csil` (long names, large enums, long
signatures), the output of `formatting/lengths.sh`, and every valid repository
spec as packages. `lengths.sh` writes one record for each field kind, with a field
for each name length from 1 to 72 characters, required and optional. A formatter
changes its line splits at exact columns, so a fixed set of long names does not
find all the lengths where the generated split is wrong. Then the check runs the
toolchain formatter of the target in check mode with default settings: rustfmt
(style editions 2021 and 2024), gofmt, `zig fmt`, `dart format`, `mix format` with
the emitted `.formatter.exs`, `dotnet format whitespace` and swift-format. Targets
without a toolchain formatter are skipped.

```bash
tests/interop/formatting/check.sh dart
```

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
| 6387 | 6388 | 6389 | 6390 | 6391 | 6392 | 6393 | 6394 | 6395 | 6396 | 6397 | 6398 | 6399 | 6400 |

## Swift harness

The Swift harness builds on Linux with a Swift 6.0 or later toolchain. It does
not use Foundation. The SwiftPM manifest is in `harness/swift/runner/`, not in
`harness/swift/`. SwiftPM gives a package the name of its directory, and the
transport package `transports/swift` already has the name `swift`. The
orchestrator runs `swift build -c release` in `runner/` and starts
`runner/.build/release/csil-interop-swift`.

If your computer has no Swift toolchain, put a `swift` script on `PATH` that
runs `swift` in the `swift:6.0-bookworm` container. The script must:

- Mount the repository at the same path in the container.
- Use `--network host`.
- Add `--static-swift-stdlib` to `swift build`, so that the binary runs on
  the host.

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

- **Scalars (`SCALARS_OK`)**: `i=-42, u=42, n=-7, f=3.5, t="héllo 世界", raw=0x0102f0ff, flag=true, due="2000-02-29", when="2026-06-29T12:34:56Z", seen="2026-06-29T12:34:56.500Z" (cddl_time, wires as tag 1 float 1782736496.5), amount="123.45", status_literal="pending" (mixed-union literal arm, wires as [1,"pending"]), status_free="unlisted" (mixed-union general arm, wires as [0,"unlisted"]), note="info" (inline mixed-choice literal arm, hoisted to synthesized `Scalars_note`, wires as [1,"info"]), size="medium" (inline all-literal choice, wires bare, no synthesis), level="high" (named enum `Level` whose last arm is `.default`-constrained; the value used deliberately differs from the declared default "medium"), season="autumn" (named enum `Season` assembled via a base rule + `/=` extension; value comes from the extension arms), ship_text="ground" (named all-literal enum `ShipMode` with MIXED literal kinds — text + int arms — wires bare as text), ship_int=2 (same `ShipMode` type, wires bare as int)`
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
| rpc        | echo-scalars/success          | 1    | round-trip every scalar incl. nint/date/timestamp/cddl_time/decimal |
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
