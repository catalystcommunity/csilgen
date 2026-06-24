# csilgen-transport (Ruby)

Reference implementation of the CSIL transport family for Ruby: **CSIL-RPC**,
**CSIL-Events**, and **CSIL-Datagrams**, over a hand-rolled canonical-CBOR codec.

This gem owns the envelope codecs, framing, and connection lifecycle. The
byte/datagram **carrier** is injected (bring-your-own-carrier), so a host plugs in
HTTP, WebSocket, QUIC, WebRTC, or a platform media stack without changing the library.
Everything is **synchronous and blocking** — no async, ever; concurrency, if a host
wants it, is plain `Thread`.

The byte layout is pinned by the shared conformance vectors in
`transports/conformance/`; this library is verified against them.

## Required toolchain

- **Ruby >= 3.2** (uses `Data.define` and frozen-by-default value objects).
- **No runtime gem dependencies.** The CBOR codec is hand-rolled, so the gem itself
  pulls in nothing at runtime.
- **Test-only gems: `minitest` and `json`.** These are *default gems* — bundled with
  most CRuby builds — but some distro packagings ship neither (notably Arch's `ruby`
  package). `run-tests.sh` detects a missing one and installs it into the per-user gem
  dir on demand (`gem install --user-install minitest json`), which needs no root and no
  Bundler but does need network access on a fresh box. Pre-install them yourself if you
  test offline. No Gemfile is involved.

## Usage

```ruby
require "csilgen/transport"

include Csilgen::Transport

# RPC client over your own frame carrier (anything responding to
# send_frame(bytes) / recv_frame -> bytes|nil):
client = RPC::Client.new(my_carrier, multiplexed: true)
response = client.call("Attestation", "deposit-claim", encoded_request_bytes)
response.into_transport_error # raises StatusError on a non-zero transport status

# RPC server: serve_one reads one request, dispatches through a handler, writes the reply.
server = RPC::Server.new(my_carrier)
server.serve_one(->(req) { RPC::Reply.new(variant: "DepositClaimResponse", payload: out_bytes) })
```

A *frame carrier* (RPC / Events) is any object responding to `send_frame(bytes)` and
`recv_frame -> bytes | nil`. A *datagram carrier* responds to `send_datagram(bytes)`
and `recv_datagram -> bytes | nil`. Built-ins: `LoopbackFrameCarrier`,
`LoopbackDatagramCarrier`, `StreamCarrier` (4-byte big-endian length framing over any
read/write/flush IO), and `UdpDatagramCarrier`.

## Consuming straight from this git repo (no publishing required)

Bundler installs a gem from a **subdirectory of a git repo** with the `:git` source plus
the `:glob` option pointing at the gemspec. This works **without publishing to
RubyGems.org**. In the consuming repo's `Gemfile`:

```ruby
gem "csilgen-transport",
    git:  "https://github.com/catalystcommunity/csilgen.git",
    glob: "transports/ruby/*.gemspec"

# pin to a branch / tag / sha as usual:
#   gem "csilgen-transport", git: "...", glob: "transports/ruby/*.gemspec", branch: "main"
#   gem "csilgen-transport", git: "...", glob: "transports/ruby/*.gemspec", tag:    "v0.1.0"
```

The gemspec `name` is `csilgen-transport`, which must equal the `gem "..."` name above.
(Known Bundler limitation: you cannot pull two *different* subdirectory gems from the
same git source in one Gemfile — irrelevant here since this repo ships a single gem.)

> **TODO (convenience publish):** publish `csilgen-transport` to RubyGems.org so
> consumers can use a plain `gem "csilgen-transport", "~> x.y"` without the git/glob
> source. Steps: bump `VERSION_STRING` + a semver git tag, `gem build
> csilgen-transport.gemspec`, `gem push csilgen-transport-<version>.gem`, and configure
> gem owners.

## Running the tests

```bash
bash transports/ruby/run-tests.sh
# or, equivalently, from transports/ruby:
ruby -Ilib -Itest test/all_tests.rb
```

The suite covers the byte-exact conformance vectors (`conformance_test.rb`) plus
loopback round-trips, framing guards, the sequence tracker, and CBOR edge cases
(`roundtrip_test.rb`).
