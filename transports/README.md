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
├── python/                       # Python reference library
├── java/        c/        csharp/    # JDK · CMake/C11 · .NET 8
├── swift/       kotlin/   zig/       # SwiftPM (UNTESTED, see below) · Gradle/JVM · Zig 0.14
├── ocaml/       elixir/   ruby/      # dune/opam · Mix · gem
    ├── dart/                            # pub (pure Dart, Flutter-compatible)
    └── php/                             # Composer package, PHP 7.2+
```

Each library ships a `run-tests.sh` (its toolchain's setup+build+test behind one
command) that `xtask test-transports` invokes, and a per-library `README.md`
documenting how to consume it straight from this git repo via that language's
package manager — with a `TODO:` wherever a published artifact will eventually be
needed (no binaries/packages are published yet).

> **Swift is UNTESTED.** Every other transport has been compiled and run against the
> conformance vectors, but no Swift toolchain was available on the dev machine (Swift on
> Linux/Arch is awkward to provision). `transports/swift/` is verified only by hand-traced
> vectors + static review; it must be built and `swift test`-ed on a real toolchain — the
> plan is a **Mac, once one can be acquired** — before it can be trusted. See
> `transports/swift/README.md`.

> **PHP is mixed-version.** `transports/php/src/Rpc.php` speaks CSIL-RPC v1 (the
> `{v, id?, service, op, payload}` / `{v, id?, status, variant?, error?, payload}`
> envelopes in `csil-rpc-transport.md`), matching Rust and Go. `Events.php` and
> `Datagrams.php` in the same package are still pre-v1 draft shapes (they key on
> `method` rather than `service`/`event` or `op`, with no `$hello` handshake or
> compact-array profile) — they predate `csil-events-transport.md` /
> `csil-datagrams-transport.md` and have not yet been migrated. `Carrier.php`
> likewise only offers a bare in-memory carrier, not the built-in
> stream/length-prefix carrier the other reference libraries provide. Do not
> treat PHP's Events/Datagrams/Carrier as spec-conformant yet.

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
suite verifies its encoders/decoders against them, which is how all the independent
implementations stay byte-compatible.

## Running the tests

```
cargo run -p xtask test-transports
```

runs every language's transport tests against the shared vectors, **skipping any
language whose toolchain is not installed** (so a partial toolchain set still runs
cleanly — only the available languages execute). Rust, Go, TypeScript, and Python
run their tools directly; every other language runs via its `transports/<lang>/run-tests.sh`:

- Rust: `cargo test -p csilgen-transport`
- Go: `cd transports/go && go test ./...`
- TypeScript: `cd transports/typescript && npm test`
- Python: `cd transports/python && python3 -m unittest discover -s tests`
- C, C#, Dart, Elixir, Java, Kotlin, OCaml, PHP, Ruby, Swift, Zig:
  `cd transports/<lang> && ./run-tests.sh`

Required toolchains (per-language, install only what you want to verify): a JDK 17+
(Java, Kotlin — Gradle wrapper is committed), .NET 8 SDK (C#), `cc`+CMake (C),
Swift 6, Zig 0.14.x, OCaml 5/dune/opam (OCaml), Elixir 1.18+/OTP 27, PHP 7.2+,
Ruby 3.2+, Dart SDK 3.5+. Each library's own `README.md` covers consumption and any publishing
`TODO:`.

`tools/install-transport-toolchains.sh` installs the download-and-extract/buildable
toolchains (Zig, a JDK for Java+Kotlin, .NET, Dart, PHP 8.x via static-php-cli, Composer) into a single per-user dir
(no root, idempotent) and writes an `env.sh` to source before `test-transports`;
it prints package-manager guidance for the four that need a compiler/system libs
(Swift, OCaml, Ruby, Elixir, exact PHP 7.x).
