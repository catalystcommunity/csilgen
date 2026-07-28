// Conventions shared by every CSIL transport — see `csil-transport-conventions.md`.
//
// The parts the three transports agree on: the CBOR rules (deterministic encoding
// lives in `cbor.dart`, tag-24 payloads here), the version constant, the transport
// status registry, the max-frame guard, the transport exception family, and the
// small map-navigation helpers the decoders share.

import 'dart:typed_data';

import 'cbor.dart';

/// Current transport version. A new value is minted only for a breaking change to
/// envelope layout or semantics.
const int version = 1;

/// CBOR semantic tag wrapping an embedded, opaque CBOR data item (RFC 8949 §3.4.5.1).
const int tagEncodedCbor = 24;

/// Reserved service ordinal for the transport control plane (Events lifecycle).
const int controlServiceOrd = 0;

/// Default max encoded envelope size for stream/message carriers (16 MiB). A
/// carrier rejects anything larger before allocating for it.
const int maxFrameDefault = 16 * 1024 * 1024;

/// Largest max-frame limit a host may configure (2 GiB - 1). The 4-byte length
/// prefix can express more, but the limit lives in a signed 32-bit integer in
/// several supported languages, so this is the portable ceiling every CSIL
/// transport agrees on. Anything above it would also disable the receive guard
/// outright, since no wire length could exceed it.
const int maxFrameLimit = 2147483647;

/// Check a host-supplied max-frame limit against the conventions doc range
/// (1..=[maxFrameLimit]), so an unusable framer fails where the host configures it
/// rather than on its first frame.
int validateMaxFrame(int maxFrame) {
  if (maxFrame < 1 || maxFrame > maxFrameLimit) {
    throw InvalidMaxFrameException(maxFrame, maxFrameLimit);
  }
  return maxFrame;
}

/// Transport-level status. Distinct from application errors, which ride inside the
/// payload as a declared `/ ErrorType` arm.
///
/// Modeled as a value type over `int` (with named constants) rather than a closed
/// `enum` so host-defined extension codes (>= 64) and unknown codes pass through
/// unchanged — an `enum` could not carry an out-of-registry code.
final class Status {
  final int code;
  const Status(this.code);

  static const Status ok = Status(0);
  static const Status malformedEnvelope = Status(1);
  static const Status unknownServiceOrOp = Status(2);
  static const Status unauthenticated = Status(3);
  static const Status forbidden = Status(4);
  static const Status versionUnsupported = Status(5);
  static const Status internal = Status(6);
  static const Status unavailable = Status(7);
  static const Status deadlineExceeded = Status(8);

  bool get isOk => code == 0;

  String get name {
    switch (code) {
      case 0:
        return 'ok';
      case 1:
        return 'malformed-envelope';
      case 2:
        return 'unknown-service-or-op';
      case 3:
        return 'unauthenticated';
      case 4:
        return 'forbidden';
      case 5:
        return 'version-unsupported';
      case 6:
        return 'internal';
      case 7:
        return 'unavailable';
      case 8:
        return 'deadline-exceeded';
      default:
        return 'other';
    }
  }

  @override
  bool operator ==(Object other) => other is Status && other.code == code;

  @override
  int get hashCode => code.hashCode;

  @override
  String toString() => 'Status($name, $code)';
}

// --- Transport exception family ---------------------------------------------
//
// These are *recoverable* wire problems, so they implement `Exception` (not
// `Error`). `on TransportException` catches the whole family.

class TransportException implements Exception {
  final String message;
  const TransportException(this.message);
  @override
  String toString() => 'TransportException: $message';
}

class EncodeException extends TransportException {
  const EncodeException(super.message);
}

class DecodeException extends TransportException {
  const DecodeException(super.message);
}

class MalformedException extends TransportException {
  const MalformedException(super.message);
}

class FrameTooLargeException extends TransportException {
  final int got;
  final int maximum;
  FrameTooLargeException(this.got, this.maximum)
      : super('frame of $got bytes exceeds max-frame guard of $maximum bytes');
}

class InvalidMaxFrameException extends TransportException {
  final int got;
  final int limit;
  InvalidMaxFrameException(this.got, this.limit)
      : super('max-frame limit of $got is outside the valid range 1..=$limit');
}

class UnsupportedVersionException extends TransportException {
  final int got;
  UnsupportedVersionException(this.got)
      : super('unsupported transport version $got');
}

class StatusException extends TransportException {
  final Status status;
  final String? detail;
  StatusException(this.status, [this.detail])
      : super('transport status ${status.name} (${status.code})'
            '${detail != null ? ': $detail' : ''}');
}

class CarrierException extends TransportException {
  CarrierException(String message) : super('carrier error: $message');
}

// --- tag-24 payloads ---------------------------------------------------------

/// Wrap opaque payload bytes (themselves a CBOR item) in tag 24.
Cbor tag24(Uint8List payload) => CborTag(tagEncodedCbor, CborBytes(payload));

/// Extract the opaque payload bytes from a tag-24 value.
Uint8List untag24(Cbor value) {
  if (value is CborTag && value.tag == tagEncodedCbor) {
    final inner = value.value;
    if (inner is CborBytes) return inner.value;
    throw const MalformedException('tag-24 payload is not a byte string');
  }
  throw const MalformedException('expected a tag-24 (encoded-cbor) payload');
}

// --- map construction & navigation -------------------------------------------

/// Build a deterministically-keyed CBOR map from text-keyed entries. The codec
/// sorts at encode time (RFC 8949 §4.2.1), so this just constructs the value.
Cbor canonMap(List<(String, Cbor)> entries) =>
    CborMap(entries.map((e) => (CborText(e.$1) as Cbor, e.$2)).toList());

Cbor? mapGet(Cbor value, String key) {
  if (value is! CborMap) return null;
  for (final (k, v) in value.entries) {
    if (k is CborText && k.value == key) return v;
  }
  return null;
}

int getUint(Cbor value, String key) {
  final f = mapGet(value, key);
  if (f is CborInt && f.value >= 0) return f.value;
  throw MalformedException("missing or non-integer field '$key'");
}

int getInt(Cbor value, String key) {
  final f = mapGet(value, key);
  if (f is CborInt) return f.value;
  throw MalformedException("missing or non-integer field '$key'");
}

String getText(Cbor value, String key) {
  final f = mapGet(value, key);
  if (f is CborText) return f.value;
  throw MalformedException("missing or non-text field '$key'");
}

String? getTextOpt(Cbor value, String key) {
  final f = mapGet(value, key);
  return f is CborText ? f.value : null;
}

int? getUintOpt(Cbor value, String key) {
  final f = mapGet(value, key);
  return (f is CborInt && f.value >= 0) ? f.value : null;
}

/// Verify a decoded envelope's version field, throwing a clear error otherwise.
void checkVersion(int v) {
  if (v != version) throw UnsupportedVersionException(v);
}

// --- byte-content equality ---------------------------------------------------

/// Compare two byte runs by content. Dart `Uint8List` equality is by reference,
/// so a decoded envelope's payload would never equal its source without this —
/// the conformance decode check relies on it.
bool bytesEqual(Uint8List? a, Uint8List? b) {
  if (identical(a, b)) return true;
  if (a == null || b == null) return false;
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

// --- hex helpers (conformance vectors + debugging) ---------------------------

String bytesToHex(Uint8List b) {
  final sb = StringBuffer();
  for (final byte in b) {
    sb.write(byte.toRadixString(16).padLeft(2, '0'));
  }
  return sb.toString();
}

Uint8List hexToBytes(String hex) {
  if (hex.length.isOdd) {
    throw const DecodeException('hex string has odd length');
  }
  final out = Uint8List(hex.length ~/ 2);
  for (var i = 0; i < out.length; i++) {
    out[i] = int.parse(hex.substring(i * 2, i * 2 + 2), radix: 16);
  }
  return out;
}
