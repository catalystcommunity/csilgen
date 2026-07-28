// Carrier seams — the bring-your-own-carrier boundary (conventions doc §7).
//
// The library owns envelope codecs, framing, and lifecycle; the byte/datagram
// transport (the *carrier*) is injected. A host supplies a WebSocket, a
// WebTransport stream, a WebRTC unreliable DataChannel, QUIC datagrams, raw UDP,
// or anything else by implementing one of these interfaces — WITHOUT changing the
// library. That is why the conformance vectors (which test the codecs above the
// seam) need no real socket.
//
// WHY THE SEAM IS SYNCHRONOUS
// ---------------------------
// Dart I/O is overwhelmingly `Future`/`Stream`-based, but the project rule is a
// synchronous, async-free codec. The seam therefore exposes *synchronous* byte
// handling. A host on an async carrier (a WebSocket, a WebTransport stream)
// bridges at the edge: it buffers inbound frames in an inbox as they arrive and
// drains them synchronously here, exactly the inbox/waiter adapter the TS
// reference documents. The conformance-tested core stays socket-free and sync;
// the host's outer event loop stays async.

import 'dart:typed_data';

import 'conventions.dart';

/// A stream/frame carrier: sends and receives one *delimited message* at a time.
/// Used by CSIL-RPC and CSIL-Events. `recvFrame` returns `null` at a clean end of
/// stream. `abstract interface class` so a host `implements` it (never `extends`).
abstract interface class FrameCarrier {
  void sendFrame(Uint8List frame);
  Uint8List? recvFrame();
}

/// A datagram carrier: sends and receives one self-contained datagram (within the
/// channel MTU), with no delivery or ordering guarantee. Used by CSIL-Datagrams.
abstract interface class DatagramCarrier {
  void sendDatagram(Uint8List datagram);
  Uint8List? recvDatagram();
}

/// Prefix `bytes` with a 4-byte big-endian length (CSIL stream framing), enforcing
/// the max-frame guard before producing the frame. The 32-bit length uses
/// `ByteData.setUint32` (safe on every backend); only the 64-bit `ByteData`
/// methods are avoided (they throw on dart2js).
Uint8List frameLengthPrefixed(Uint8List bytes, {int max = maxFrameDefault}) {
  validateMaxFrame(max);
  if (bytes.length > max) throw FrameTooLargeException(bytes.length, max);
  final out = Uint8List(4 + bytes.length);
  ByteData.sublistView(out).setUint32(0, bytes.length, Endian.big);
  out.setRange(4, 4 + bytes.length, bytes);
  return out;
}

/// An incremental deframer for the 4-byte big-endian length-prefix wire. A host
/// wiring a byte stream feeds inbound chunks via [push] and drains complete frames
/// via [next]. The max-frame guard rejects a hostile length prefix BEFORE
/// buffering its claimed body, so a corrupt prefix cannot exhaust memory.
class LengthPrefixedDeframer {
  Uint8List _buf = Uint8List(0);
  final int max;

  /// The limit is validated here rather than at the first frame, so a
  /// misconfigured deframer is a construction-time error the host can surface at
  /// startup.
  LengthPrefixedDeframer({int max = maxFrameDefault})
      : max = validateMaxFrame(max);

  void push(Uint8List chunk) {
    final merged = Uint8List(_buf.length + chunk.length);
    merged.setRange(0, _buf.length, _buf);
    merged.setRange(_buf.length, merged.length, chunk);
    _buf = merged;
  }

  /// The next complete frame, or `null` if not enough bytes have arrived.
  Uint8List? next() {
    if (_buf.length < 4) return null;
    final len = ByteData.sublistView(_buf, 0, 4).getUint32(0, Endian.big);
    if (len > max) throw FrameTooLargeException(len, max);
    if (_buf.length < 4 + len) return null;
    final frame = Uint8List.fromList(Uint8List.sublistView(_buf, 4, 4 + len));
    _buf = Uint8List.fromList(Uint8List.sublistView(_buf, 4 + len));
    return frame;
  }
}

/// An in-memory FrameCarrier backed by frame queues — for tests and for driving
/// the codec without a socket. `outbound` collects what was sent; `inbound` feeds
/// recv.
final class LoopbackFrameCarrier implements FrameCarrier {
  final List<Uint8List> outbound = [];
  final List<Uint8List> inbound = [];

  void pushInbound(Uint8List bytes) => inbound.add(bytes);
  Uint8List? takeOutbound() => outbound.isEmpty ? null : outbound.removeAt(0);

  @override
  void sendFrame(Uint8List frame) => outbound.add(frame);

  @override
  Uint8List? recvFrame() => inbound.isEmpty ? null : inbound.removeAt(0);
}

/// An in-memory DatagramCarrier — for tests and codec drives.
final class LoopbackDatagramCarrier implements DatagramCarrier {
  final List<Uint8List> outbound = [];
  final List<Uint8List> inbound = [];

  void pushInbound(Uint8List bytes) => inbound.add(bytes);
  Uint8List? takeOutbound() => outbound.isEmpty ? null : outbound.removeAt(0);

  @override
  void sendDatagram(Uint8List datagram) => outbound.add(datagram);

  @override
  Uint8List? recvDatagram() => inbound.isEmpty ? null : inbound.removeAt(0);
}
