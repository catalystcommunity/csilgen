# csilgen-transport (Zig)

The Zig implementation of the CSIL transport family: a hand-rolled, dependency-free
canonical-CBOR codec plus matched client/server for all three CSIL transports —
**RPC**, **Events**, and **Datagrams** — over a bring-your-own-carrier seam.

It is the Zig sibling of `transports/rust`, `transports/go`, `transports/python`, and
`transports/typescript`. The wire format is fixed by
`transports/conformance/{rpc,events,datagrams}.json`; this library is verified
byte-for-byte against those vectors.

## Toolchain: Zig 0.14.1 (pinned)

Zig has no stable 1.0 and breaks the `std` / build APIs nearly every minor release.
This library is written against the **0.14 line** and pins `minimum_zig_version =
"0.14.1"` in `build.zig.zon`.

- It uses the **0.14 managed `std.ArrayList`** and **blocking `std.io`**.
- **0.15+** made `std.ArrayList` unmanaged by default and reworked `std.io`; **0.13**
  has the older `addExecutable`/`addTest` build API. Neither builds this unchanged.
- The version-sensitive surface is intentionally small and localized (the
  `std.ArrayList(u8)` buffers in `src/cbor.zig` / `src/datagrams.zig`, `std.mem.writeInt`/
  `readInt`, and `std.net.Stream` in `src/carrier.zig`), so a future 0.15 port is a
  handful of edits rather than a rewrite.

Install Zig 0.14.1 from <https://ziglang.org/download/> (a single statically linked
binary; no system libraries are required for this pure-Zig library).

## Building and testing

```sh
cd transports/zig
zig build test     # unit tests + conformance vectors
```

or run the bundled script (what the repo's `xtask` invokes; it checks for a 0.14
toolchain and exits non-zero on any failure):

```sh
./run-tests.sh
```

Tests are Zig's built-in `test {}` blocks run by `zig build test` — no external test
framework. The conformance vectors in `transports/conformance/*.json` are read at
**build time** (see `build.zig`) and injected into the conformance test as
compile-time strings, so the tests do not depend on the run step's working directory.

## Layout

| file | purpose |
|------|---------|
| `src/root.zig` | package root; re-exports every submodule |
| `src/cbor.zig` | canonical CBOR encode/decode (RFC 8949), hand-rolled, zero deps |
| `src/conventions.zig` | version, status registry, tag-24 wrap/unwrap, field accessors |
| `src/carrier.zig` | BYO-carrier seam (`*anyopaque` + vtable) + loopback / length-prefixed stream carriers |
| `src/rpc.zig` | CSIL-RPC request/response/push + client/server |
| `src/events.zig` | CSIL-Events verbose/compact frames + control-plane lifecycle |
| `src/datagrams.zig` | CSIL-Datagrams cbor-array + compact-header profiles + sequence tracker |

## Using it from your project

Downstreams import the module as `csilgen_transport`.

```zig
const csil = @import("csilgen_transport");

var req = csil.rpc.RpcRequest.init("Attestation", "deposit-claim", payload_bytes);
req.id = 7;
const frame = try req.encode(allocator);   // caller frees with allocator.free
defer allocator.free(frame);
```

### Memory ownership

There is no GC; ownership is explicit and documented per function:

- **Encode** returns an owned `[]u8` — free it with `allocator.free`.
- **Decode** allocates the decoded value tree (and the byte/text slices the result
  aliases) from the allocator you pass in. **Pass a `std.heap.ArenaAllocator`** and
  free the whole tree in one `arena.deinit()`. The decoded struct's `[]const u8`
  fields point into that arena, so keep it alive while you use them.
- **Carriers** are blocking and synchronous. No `async`, ever — the host owns the I/O
  loop; use `std.Thread` for concurrency. `recv` allocates the frame into a
  caller-passed allocator.

When acting as a server, free the request's decode arena **after** you encode the
response: a handler's reply payload may alias the decoded request's storage.

## Consuming this library via the Zig package manager

Zig's package manager consumes a git repository directly. From your project root:

```sh
zig fetch --save "git+https://github.com/catalystcommunity/csilgen.git#<commit-sha>"
```

`zig fetch` clones, computes the content hash, and writes the `.url` + `.hash` into
your `build.zig.zon` `.dependencies`. Always pin `#<commit-sha>` (not a bare branch)
so the hash is reproducible — packages are identified by hash, not URL.

Then in your `build.zig`:

```zig
const dep = b.dependency("csilgen_transport", .{ .target = target, .optimize = optimize });
exe.root_module.addImport("csilgen_transport", dep.module("csilgen_transport"));
```

### TODO: monorepo-subdirectory fetch is not guaranteed on Zig 0.14

This package lives at `transports/zig/`, **not** at the repo root (the repo root is a
Rust workspace). A `git+https` URL fetches the repository root, and reliable selection
of a package in a **subdirectory** of a git repo is not guaranteed on the pinned
0.14.1 (`zig fetch` keys off where it finds a `build.zig.zon`; subdir selection from a
git URL has historically been awkward — see ziglang/zig#21645).

Until this is confirmed working on 0.14.1, the **blessed consumption path** is a URL
whose archive root *is* this Zig package:

- a tagged release or a `git+https`/tarball URL whose extracted root is
  `transports/zig/` (e.g. a release tarball built from this subdirectory), **or**
- a thin split-mirror repository that contains only the Zig package at its root.

This `build.zig.zon` already scopes `paths` to just this package, so it is ready to be
the root of such an archive. No registry step is needed — Zig has no central registry;
git/tarball is the native distribution channel. **We do not publish any binaries or
packages yet**; publishing the tag/tarball above is the only step a maintainer must add
later to make subdir-free `zig fetch --save` work for consumers.
