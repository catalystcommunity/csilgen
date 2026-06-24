# csilgen-transport (OCaml)

A hand-written, dependency-free OCaml implementation of the CSIL transport wire —
the [conventions doc](../../docs/csil-transport-conventions.md) plus the
[RPC](../../docs/csil-rpc-transport.md),
[Events](../../docs/csil-events-transport.md), and
[Datagrams](../../docs/csil-datagrams-transport.md) transports.

It is the OCaml sibling of `transports/go`, `transports/python`, `transports/rust`,
and `transports/typescript`, verified against the same byte-exact conformance
vectors in `transports/conformance/`.

- **dune library name:** `csilgen_transport` (modules `Csilgen_transport.Cbor`,
  `…Conventions`, `…Carrier`, `…Rpc`, `…Events`, `…Datagrams`, `…Udp`).
- **opam package name:** `csilgen-transport`.
- **No async.** Everything is synchronous and blocking; the host owns the I/O loop
  and any threads. No Lwt/Async/Eio.
- **No third-party deps.** Only the stdlib and the compiler-bundled `unix`
  (UDP carrier only). The conformance test target is stdlib-only — it ships its own
  ~200-line JSON reader rather than pulling `yojson`.

## Toolchain

You need only the **OCaml 5.x** compiler (works back to 4.08 for the stdlib
`Bytes.*_be` accessors) and **dune ≥ 3.0**, plus **opam** to consume it as a
package. The simplest install:

```sh
opam switch create . 5.1.1   # or any 5.x; reuses an existing switch if present
opam install dune
```

No system packages beyond the OCaml toolchain itself.

## Build & test

```sh
cd transports/ocaml
dune build
CSILGEN_CONFORMANCE_DIR="$(pwd)/../conformance" dune runtest
```

Or just run the wrapper (what xtask invokes), which sets the vector path for you:

```sh
./run-tests.sh
```

The conformance vectors live at `transports/conformance/`, which is **outside** this
dune project, so the test locates them via `CSILGEN_CONFORMANCE_DIR`. `run-tests.sh`
exports it automatically; if you call `dune runtest` directly, set it yourself (a
couple of relative fallbacks cover the common in-tree layout).

## Consuming it from this git repo

We do not publish binaries or an opam package yet. Pick whichever fits your project.

### Option A — `opam pin` to the subdirectory (recommended)

`csilgen-transport.opam` and `dune-project` live in `transports/ocaml/`, a
subdirectory of the repo, so pin with `--subpath` (opam 2.1+):

```sh
opam pin add csilgen-transport.dev \
  "git+https://github.com/catalystcommunity/csilgen.git#main" \
  --subpath transports/ocaml
```

Pin to a tag or commit instead of `main` for reproducibility.

### Option B — `pin-depends` in your own opam file (declarative)

Record the pin in your consumer package's `.opam`:

```
depends: [ "csilgen-transport" ]
pin-depends: [
  [ "csilgen-transport.dev"
    "git+https://github.com/catalystcommunity/csilgen.git#main" ]
]
```

`pin-depends` is not transitive (it only resolves your package's own direct
dependency), so Option A's `--subpath` remains the cleanest cross-repo path; this is
a convenience for keeping the pin in-tree.

### Option C — dune `(vendored_dirs …)` (no opam at all)

If you vendor or git-submodule this repo (or just `transports/ocaml/`) under your own
dune project, mark it vendored in your top-level `dune`:

```
(vendored_dirs csilgen)   ; or whatever you named the vendored subdir
```

Dune then builds `csilgen_transport` from source and suppresses
warnings-as-errors for the vendored tree. This is the most robust offline path.

## TODO (publishing)

> **Not yet published to opam-repository.** To enable plain
> `opam install csilgen-transport`, add this package to
> [`ocaml/opam-repository`](https://github.com/ocaml/opam-repository) (or a private
> opam-repo). Until then, use Option A (`opam pin --subpath transports/ocaml`) or
> Option C (dune `vendored_dirs`).

## Layout

```
transports/ocaml/
├── dune-project
├── dune                       # env: warnings non-fatal (toolchain not always present)
├── csilgen-transport.opam
├── lib/
│   ├── cbor.ml/.mli           # canonical CBOR codec (Buffer + Bytes.*_be, Int64 ints)
│   ├── conventions.ml/.mli    # version, status registry, tag-24, field accessors
│   ├── carrier.ml/.mli        # BYO seam (records of closures) + loopback + length-prefix
│   ├── udp.ml/.mli            # UDP datagram carrier (uses bundled unix)
│   ├── rpc.ml/.mli            # request / response / push + client / server
│   ├── events.ml/.mli         # verbose + compact events, control-plane payloads
│   └── datagrams.ml/.mli      # cbor-array + compact-header datagrams, seq tracker
└── test/
    ├── json.ml                # tiny stdlib JSON reader (test-only)
    ├── conformance.ml         # byte-exact vector checks
    ├── roundtrip.ml           # loopback + seq tracker + Int64 boundary checks
    └── run_tests.ml           # entry point for the dune (test) stanza
```

Every `.ml` in `lib/` has a `.mli`: the interface is the abstraction boundary that
keeps the in-memory CBOR model, head writer, and decode cursor private.

## API sketch

```ocaml
open Csilgen_transport

(* RPC client over a bring-your-own frame carrier *)
let client = Rpc.new_client my_frame_carrier (* multiplexed: *) true in
match Rpc.call client ~service:"Attestation" ~op:"deposit-claim" ~payload () with
| Ok resp -> (* resp.payload is the opaque CBOR(output) bytes *) ...
| Error e -> prerr_endline (Conventions.error_message e)

(* RPC server: the host supplies a request -> outcome handler *)
let server = Rpc.new_server my_frame_carrier in
let _ : (bool, _) result =
  Rpc.serve_one server (fun req ->
    Rpc.reply "DepositClaimResponse" (handle req.payload))
```

A carrier is just a record of closures, so a host plugs in QUIC/WebRTC/WebSocket by
building one — no library edit:

```ocaml
let my_frame_carrier : Carrier.frame_carrier =
  { send_frame = (fun bytes -> ...; Ok ());
    recv_frame = (fun () -> ... (* Ok None at clean EOF *)) }
```
