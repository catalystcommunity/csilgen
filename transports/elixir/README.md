# csilgen_transport (Elixir)

A synchronous, dependency-free Elixir implementation of the CSIL transport
layer: matched client/server envelopes for all three CSIL transports — **RPC**,
**Events**, and **Datagrams** — over a hand-rolled **canonical CBOR** codec
(RFC 8949 deterministic encoding). It is verified byte-for-byte against the
shared conformance vectors in `transports/conformance/`.

- **App:** `:csilgen_transport`  ·  **Modules:** `Csilgen.Transport.*`
- **Zero external dependencies.** The CBOR codec is hand-rolled; the conformance
  tests use the OTP-native `JSON` module and stdlib `Base`.
- **No async, ever.** Encode/decode are pure functions of binaries (no
  GenServers, no processes on the codec path). The host owns the I/O loop and
  injects a carrier.

## Toolchain

**Elixir 1.18+ on Erlang/OTP 27+.** The floor is set by the native `JSON`
module the conformance tests rely on; the library proper needs nothing newer. No
C toolchain or native compilation — pure Elixir.

## Modules

| Module | Responsibility |
| --- | --- |
| `Csilgen.Transport.CBOR` | Canonical CBOR encode/decode (uint/nint/bstr/tstr/array/map/tag) |
| `Csilgen.Transport.Conventions` | Version, tag-24 payloads, status, 16 MiB frame guard, map accessors |
| `Csilgen.Transport.Status` | Transport status registry (codes + names) |
| `Csilgen.Transport.Carrier` | Bring-your-own-carrier behaviour + length-prefix framing + `Loopback` |
| `Csilgen.Transport.RPC` | `Request` / `Response` / `Push` envelopes |
| `Csilgen.Transport.Events` | Verbose + compact events and the `$hello`/`$ping`/`$close` control plane |
| `Csilgen.Transport.Datagrams` | `cbor-array` + `compact-header` datagrams and the sequence tracker |

### The carrier seam

`Csilgen.Transport.Carrier` is a `@behaviour` with `send_frame/2` and
`recv_frame/1`. The first argument is an **opaque, host-owned term** (a socket
ref, a struct, a pid) — the library never spawns it, and every call is
synchronous. Each call returns the (possibly updated) carrier so an immutable
carrier can be threaded functionally; `Csilgen.Transport.Carrier.Loopback` is a
pid-free in-memory carrier for tests and for driving the codec without a socket.

## Consuming this library (no publishing required)

This works directly from the git repo via Mix — **no Hex publish needed.** Mix
consumes a subdirectory of a monorepo with the `:sparse` option (it fetches only
that directory). Add to your `mix.exs`:

```elixir
def deps do
  [
    {:csilgen_transport,
     git: "https://github.com/catalystcommunity/csilgen.git",
     sparse: "transports/elixir",
     branch: "main"}   # or tag: "vX.Y.Z" / ref: "<40-char-sha>"
  ]
end
```

`:sparse` is orthogonal to `:branch`/`:tag`/`:ref` and combines freely. Sparse
checkout needs a reasonably modern `git` on the consuming machine. The dep atom
(`:csilgen_transport`) must match this project's app name — it does.

> **TODO (Hex publishing deferred):** No packages are published yet. To publish
> later, fill in `links` in `mix.exs`'s `package/0` and run `mix hex.publish`.
> Until then, the git+sparse dep above is the supported path.

## Running the tests

```sh
./run-tests.sh        # mix deps.get && mix test  (exits non-zero on failure)
# or, directly:
mix test
```

The conformance suite (`test/conformance_test.exs`) drives
`transports/conformance/{rpc,events,datagrams}.json`: for every vector it
rebuilds the envelope from `input`, asserts `encode → hex`, and asserts
`decode(hex) → the same envelope`. `test/roundtrip_test.exs` covers codec
primitives, the loopback carrier, the framing guard, and sequence-tracker
semantics.
