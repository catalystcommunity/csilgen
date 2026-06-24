# csilgen_transport (Dart)

Reference **Dart** implementation of the CSIL transport family — CSIL-RPC,
CSIL-Events, and CSIL-Datagrams — over a hand-rolled canonical-CBOR codec.

It is **pure Dart with zero runtime dependencies and no Flutter import**, so the
same package resolves and runs under the standalone `dart` toolchain, on a Dart
server, and inside a Flutter app — including **Flutter Web (dart2js)**. The codec
touches only `dart:typed_data`, `dart:convert`, and `BigInt`.

## What it provides

- `lib/src/cbor.dart` — canonical (RFC 8949 §4.2.1 core-deterministic) CBOR
  encode/decode, hand-rolled. Maps are emitted from a **sorted entry list**
  (bytewise order of encoded keys), never `Map` insertion order.
- `lib/src/conventions.dart` — tag-24 payloads, the transport `Status` registry,
  the version constant, the 16 MiB frame guard, the exception family, and
  content-based byte equality (`Uint8List` equality is by reference in Dart, so a
  decoded payload would never compare equal without it).
- `lib/src/carrier.dart` — the bring-your-own-carrier seam
  (`abstract interface class FrameCarrier` / `DatagramCarrier`, **synchronous** byte
  methods), length-prefix framing + an incremental deframer, and in-memory loopback
  carriers. Real WebSocket / WebTransport / WebRTC / UDP carriers are documented as
  thin host-side adapters; the library code never changes.
- `lib/src/rpc.dart`, `events.dart`, `datagrams.dart` — the three transports.

Public surface is the barrel `lib/csilgen_transport.dart`; everything under
`lib/src/` is private-by-convention.

```dart
import 'package:csilgen_transport/csilgen_transport.dart';
```

### The 64-bit web trap (why there is no `setUint64`)

On Flutter Web / dart2js, `ByteData.setUint64`/`getUint64` **throw**
`UnsupportedError`, and a Dart `int` is a 53-bit JS double. This codec therefore
**never** calls the 64-bit `ByteData` methods: the 8-byte (major-info 27) CBOR head
is assembled and disassembled by hand with byte shifts and `BigInt`. A decoded
integer above `2^53 - 1` is rejected with `DecodeException` rather than silently
truncated (no envelope field needs a value that large). If a future field genuinely
needs full 64-bit width, `package:fixnum`'s `Int64` is the documented escape hatch.

## Consuming it straight from this git repo (no publishing)

pub supports a git dependency pointing at a **subdirectory** of a repo via the
`path` key — no publishing required. In a consumer's `pubspec.yaml`:

```yaml
dependencies:
  csilgen_transport:
    git:
      url: https://github.com/catalystcommunity/csilgen.git
      path: transports/dart   # relative to the repo root
      ref: main               # pin a tag/commit for reproducible resolution
```

This works identically for `dart pub get` and Flutter's `flutter pub get`. The
package is kept self-contained with **no path dependencies**, which avoids the known
pub limitation where a git package's relative `path:` dep fails to resolve.

> **TODO (publishing — deferred):** publishing to pub.dev for
> `csilgen_transport: ^1.0.0` ergonomics would require an unclaimed package name,
> `homepage`/`repository` metadata, a `CHANGELOG.md`, the existing Apache-2.0
> license, and a clean `dart pub publish --dry-run`. None of this blocks git-based
> consumption today.

## Toolchain & tests

- **Toolchain required:** the **Dart SDK ≥ 3.5** (provides `dart pub`, `dart
  analyze`, `dart format`, `dart test`). No other system packages. The SDK floor is
  3.5 because the library uses sealed classes and patterns (Dart ≥ 3.0); 3.5 is a
  safe modern floor.
- **Run the tests** (full setup + run, exits non-zero on failure):

  ```sh
  ./run-tests.sh        # == dart pub get && dart test
  ```

  `test/conformance_test.dart` drives the shared byte-exact vectors in
  `transports/conformance/{rpc,events,datagrams}.json`; `test/roundtrip_test.dart`
  covers codec edge cases (including the web-safe 64-bit head path), canonical map
  ordering, framing + the frame guard, a loopback RPC exchange, and sequence
  tracking.

The dev-only dependencies (`test`, `lints`) are dropped transitively from any
consumer's resolution, so depending on this package pulls in nothing at runtime.
