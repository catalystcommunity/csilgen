# CsilgenTransport (Swift)

A hand-written Swift implementation of the CSIL transport family — **CSIL-RPC**,
**CSIL-Events**, and **CSIL-Datagrams** — over a dependency-free, canonical CBOR codec.
Matched client + server for each transport, with a bring-your-own-carrier seam so the host
owns the I/O loop.

This is the Swift sibling of `transports/{go,python,rust,typescript,c}`. The wire format is
fixed by the specs in `docs/` and the byte-exact fixtures in `transports/conformance/`,
which the test suite checks against.

> **⚠️ Status: UNTESTED.** Unlike every other transport in this repo, this library has **not
> yet been compiled or run** — no Swift toolchain was available on the development machine
> (Swift on Linux/Arch is awkward to provision). It has been verified only by hand-tracing
> the conformance vectors and by static review, so treat it as *unproven* until it is built
> and `swift test`-ed on a real Swift toolchain — the plan is to validate it on a **Mac once
> one can be acquired** for the purpose. Any "passes the vectors" claim here is aspirational
> until then.

## Design rules (non-negotiable, inherited from the repo)

- **No async, ever.** Every carrier method and every client/server call is a plain,
  blocking `func ... throws`. Concurrency, when a host wants it, is *threads* — the library
  never spawns a `Task`, never marks anything `async`. Do not "modernize" this with
  SwiftNIO / `URLSession` / `async-await`; that is a deliberate constraint, not an
  oversight.
- **Zero external dependencies.** The package declares no `dependencies:` at all. The
  codec, conventions, carriers, and the whole conformance surface touch only the Swift
  **standard library** — no `Foundation`. Everything is `[UInt8]`, `UInt64`, bit-shifts,
  and `String.utf8`, identical on Linux and Apple. (`Foundation` appears only in the test
  target, for JSON parsing of the vector files.)
- **Canonical CBOR.** Map entries are sorted by the bytewise order of their *encoded keys*
  (length-prefixed, so shorter keys sort first), integers use shortest form, and payloads
  are tag-24 byte strings. This is what makes the bytes match the conformance vectors.

## Layout

```
transports/swift/
├── Package.swift
├── Sources/CsilgenTransport/
│   ├── CBOR.swift          # canonical encode/decode over [UInt8]
│   ├── Conventions.swift   # version, Status registry, tag-24, frame guard, field accessors
│   ├── Carrier.swift       # FrameCarrier / DatagramCarrier seams + loopback + length-prefix
│   ├── RPC.swift           # RpcRequest/Response/Push, RpcClient, RpcServer
│   ├── Events.swift        # verbose + compact event frames, $hello/$ping/$close control plane
│   └── Datagrams.swift     # cbor-array + compact-header datagrams, SeqTracker
└── Tests/CsilgenTransportTests/
    ├── ConformanceTests.swift   # drives transports/conformance/{rpc,events,datagrams}.json
    └── RoundtripTests.swift     # dispatch, framing, SeqTracker, guards, map ordering
```

## Building and testing

Requires a **Swift 6.0+ toolchain** on `PATH` and nothing else (no system packages, no
networking libraries — the conformance surface is stdlib-only).

```sh
cd transports/swift
swift build
swift test          # or: ./run-tests.sh   (what xtask invokes)
```

`run-tests.sh` is the single entry point the repo's xtask runner calls; it exits non-zero
on any test failure and is skipped automatically when `swift` is not installed.

`Status` is modeled as a `struct` over an `Int` (not a closed enum) so host-defined
extension codes (≥ 64) and otherwise-unknown codes round-trip verbatim.

## Consuming from this git repo — the SwiftPM subdirectory blocker

SwiftPM resolves a *git-URL* dependency (`.package(url:)`) by cloning the repo and looking
for `Package.swift` **at the repository root**. There is no parameter to point a git-URL
dependency at a subdirectory, so a package living at `transports/swift/` **cannot** be
consumed via `.package(url: ".../csilgen.git", …)` directly. (This was pitched in
swift-package-manager#5768 and never implemented.)

### Supported route today: local path dependency (clone or submodule)

SwiftPM *does* accept a subdirectory for a **path** dependency. Check out (or submodule)
csilgen, then point at the subdirectory by path:

```swift
// In the consumer's Package.swift
dependencies: [
    .package(path: "../csilgen/transports/swift"),
],
// …
.target(name: "App", dependencies: [
    .product(name: "CsilgenTransport", package: "CsilgenTransport"),
]),
```

Or as a submodule:

```sh
git submodule add https://github.com/catalystcommunity/csilgen.git Vendor/csilgen
# then .package(path: "Vendor/csilgen/transports/swift")
```

### TODO (publishing): enable one-line git-URL consumption

To allow `.package(url: ".../csilgen.git", branch: "main")` directly, a future step must do
**one** of:

1. **Add a root-level `Package.swift`** at the csilgen repo root whose target uses
   `path: "transports/swift/Sources/CsilgenTransport"`. Cost: a Swift manifest at the root
   of a Rust/Cargo monorepo, and the package identity becomes `csilgen`.
2. **Mirror `transports/swift/` to its own repo** (e.g. `csilgen-swift-transport`) with
   `Package.swift` at *its* root, via a CI subtree push.
3. **Publish to a Swift Package Registry** once one is adopted.

Until then, use the local-path / submodule route above. Pin syntax for reference, once a
root manifest or split repo exists: `.package(url: u, from: "1.0.0")`,
`.package(url: u, branch: "main")`, `.package(url: u, revision: "<sha>")`,
`.package(url: u, exact: "1.0.0")`.
