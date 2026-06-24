// csilgen_transport (Dart) — reference implementation of the CSIL transport
// family: CSIL-RPC, CSIL-Events, and CSIL-Datagrams. It owns the envelope codecs,
// framing, and connection lifecycle; the byte/datagram carrier is injected
// (bring-your-own-carrier), so a host plugs in a WebSocket, WebTransport, a WebRTC
// unreliable DataChannel, QUIC, raw UDP, or anything else without changing this
// library.
//
// Pure Dart: the codecs touch only `dart:typed_data`, `dart:convert`, and
// `BigInt`. There is no Flutter dependency and no `dart:io`, so the package
// resolves and runs identically on the Dart VM, on a server, and inside a Flutter
// app — including Flutter Web (dart2js), where the 64-bit `ByteData` methods would
// throw. See `lib/src/cbor.dart` for the web-safe 8-byte head assembly.
//
// See the spec docs at the repo root: csil-transport-conventions.md,
// csil-rpc-transport.md, csil-events-transport.md, csil-datagrams-transport.md.
// The byte layout is pinned by the conformance vectors in transports/conformance/.

export 'src/cbor.dart';
export 'src/conventions.dart';
export 'src/carrier.dart';
export 'src/rpc.dart';
export 'src/events.dart';
export 'src/datagrams.dart';
