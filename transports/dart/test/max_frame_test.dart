// The configurable max-frame guard (conventions doc §5): a host sets the limit up
// or down where the framing is configured, the limit applies to outbound frames and
// inbound length prefixes alike, an oversized prefix is rejected before its body is
// buffered, and an invalid limit fails where it is configured rather than on the
// first frame.
//
// Dart has no stateful stream carrier (a host wires its own socket or WebSocket
// source), so the knob lives on `frameLengthPrefixed` for the write side and on the
// `LengthPrefixedDeframer` constructor for the read side.

import 'dart:typed_data';

import 'package:csilgen_transport/csilgen_transport.dart';
import 'package:test/test.dart';

/// A 4-byte big-endian prefix, used to feed the deframer a claim without a body.
Uint8List prefixFor(int len) {
  final out = Uint8List(4);
  ByteData.sublistView(out).setUint32(0, len, Endian.big);
  return out;
}

void main() {
  group('max-frame guard', () {
    test('default limit accepts a frame below it', () {
      final body = Uint8List(1024)..fillRange(0, 1024, 0xAB);
      final framed = frameLengthPrefixed(body);
      expect(framed.length, 4 + body.length);

      final deframer = LengthPrefixedDeframer();
      deframer.push(framed);
      expect(deframer.next(), body);
    });

    test('default limit rejects a frame above it', () {
      final body = Uint8List(maxFrameDefault + 1);
      expect(
        () => frameLengthPrefixed(body),
        throwsA(isA<FrameTooLargeException>()),
      );
    });

    test('a larger custom limit accepts what the default rejects', () {
      final body = Uint8List(maxFrameDefault + 1);
      final framed = frameLengthPrefixed(body, max: maxFrameDefault + 4096);
      expect(framed.length, 4 + body.length);

      final deframer = LengthPrefixedDeframer(max: maxFrameDefault + 4096);
      deframer.push(framed);
      expect(deframer.next()!.length, body.length);
    });

    test('a smaller custom limit rejects what the default accepts', () {
      final body = Uint8List(1024)..fillRange(0, 1024, 0xCD);
      expect(
        () => frameLengthPrefixed(body, max: 64),
        throwsA(isA<FrameTooLargeException>()),
      );
    });

    test('an oversized incoming length is rejected before its body is buffered', () {
      // Only the 4-byte prefix is pushed — no body at all. The guard must fire on
      // the claim itself rather than waiting for bytes that will never arrive.
      final deframer = LengthPrefixedDeframer(max: 4096);
      deframer.push(prefixFor(0xFFFFFFFF));
      expect(deframer.next, throwsA(isA<FrameTooLargeException>()));
    });

    test('invalid limits are rejected where they are configured', () {
      for (final limit in [0, -1, -4096, maxFrameLimit + 1, 1 << 40]) {
        expect(
          () => LengthPrefixedDeframer(max: limit),
          throwsA(isA<InvalidMaxFrameException>()),
          reason: 'deframer limit $limit must be rejected',
        );
        expect(
          () => frameLengthPrefixed(Uint8List(1), max: limit),
          throwsA(isA<InvalidMaxFrameException>()),
          reason: 'framing limit $limit must be rejected',
        );
      }
    });

    test('boundary limits are accepted', () {
      for (final limit in [1, maxFrameDefault, maxFrameLimit]) {
        expect(LengthPrefixedDeframer(max: limit).max, limit);
      }
    });
  });
}
