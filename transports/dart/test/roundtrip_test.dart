// Behavioral tests beyond the byte-exact conformance vectors: codec edge cases
// (including the web-safe 64-bit head path), canonical map ordering, framing and
// the frame guard, the loopback client/server round trip, and sequence tracking.

import 'dart:typed_data';

import 'package:csilgen_transport/csilgen_transport.dart';
import 'package:test/test.dart';

Uint8List bytes(List<int> b) => Uint8List.fromList(b);

void main() {
  group('CBOR codec', () {
    test('integer head widths are the shortest form', () {
      expect(bytesToHex(encode(const CborInt(0))), equals('00'));
      expect(bytesToHex(encode(const CborInt(23))), equals('17'));
      expect(bytesToHex(encode(const CborInt(24))), equals('1818'));
      expect(bytesToHex(encode(const CborInt(255))), equals('18ff'));
      expect(bytesToHex(encode(const CborInt(256))), equals('190100'));
      expect(bytesToHex(encode(const CborInt(65535))), equals('19ffff'));
      expect(bytesToHex(encode(const CborInt(65536))), equals('1a00010000'));
      expect(
          bytesToHex(encode(const CborInt(4294967295))), equals('1affffffff'));
      // 2^32 forces the 8-byte (major-info 27) head, assembled by hand/BigInt —
      // never via ByteData.setUint64, which throws on dart2js.
      expect(bytesToHex(encode(const CborInt(4294967296))),
          equals('1b0000000100000000'));
    });

    test('negative integers use major type 1', () {
      expect(bytesToHex(encode(const CborInt(-1))), equals('20'));
      expect(bytesToHex(encode(const CborInt(-24))), equals('37'));
      expect(bytesToHex(encode(const CborInt(-256))), equals('38ff'));
    });

    test('8-byte head round-trips through the BigInt path', () {
      // 2^48: well past 2^32 so it exercises major-info 27, but under 2^53 so it
      // survives as a web int.
      const big = 0x1000000000000;
      final wire = encode(const CborInt(big));
      expect(wire[0], equals(0x1b));
      final back = decode(wire);
      expect(back, isA<CborInt>());
      expect((back as CborInt).value, equals(big));
    });

    test('decode rejects an 8-byte integer above the safe range', () {
      // 0xffffffffffffffff — a 64-bit value no web int can hold.
      final wire =
          bytes([0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
      expect(() => decode(wire), throwsA(isA<DecodeException>()));
    });

    test('negative-int decode guard: -(2^53) is the floor that still decodes',
        () {
      // Major type 1 with arg = 2^53 - 1 encodes -(2^53), the smallest value a
      // web int holds exactly; anything more negative must be rejected.
      final floor = encode(const CborInt(-9007199254740992));
      expect((decode(floor) as CborInt).value, equals(-9007199254740992));
      // arg = 2^53 (-(2^53)-1) is one past the floor and must throw.
      final tooNegative =
          bytes([0x3b, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
      expect(() => decode(tooNegative), throwsA(isA<DecodeException>()));
    });

    test('map entries are sorted by encoded-key bytes, not insertion order',
        () {
      // Keys deliberately out of canonical order; "a" (61) < "b" (62) < "cc".
      final m = CborMap([
        (const CborText('b'), const CborInt(2)),
        (const CborText('cc'), const CborInt(3)),
        (const CborText('a'), const CborInt(1)),
      ]);
      // a3 (map,3) 6161 01 6162 02 626363 03
      expect(bytesToHex(encode(m)), equals('a361610161620262636303'));
    });

    test('shorter key sorts before a longer key with the same prefix', () {
      final m = CborMap([
        (const CborText('aa'), const CborInt(2)),
        (const CborText('a'), const CborInt(1)),
      ]);
      expect(bytesToHex(encode(m)), equals('a261610162616102'));
    });

    test('trailing bytes after a complete item are rejected', () {
      expect(
          () => decode(bytes([0x00, 0x00])), throwsA(isA<DecodeException>()));
    });

    test('indefinite-length / reserved additional-info is rejected', () {
      expect(() => decode(bytes([0x5f])), throwsA(isA<DecodeException>()));
    });

    test('text round-trips UTF-8', () {
      final wire = encode(const CborText('héllo'));
      expect((decode(wire) as CborText).value, equals('héllo'));
    });
  });

  group('conventions', () {
    test('tag-24 wraps and unwraps payload bytes', () {
      final payload = bytes([0xa0]);
      final wrapped = tag24(payload);
      expect(bytesEqual(untag24(wrapped), payload), isTrue);
    });

    test('empty payload is a present tag-24 wrapping empty bytes', () {
      final wire = encode(tag24(Uint8List(0)));
      expect(bytesToHex(wire), equals('d81840'));
      expect(untag24(decode(wire)).length, equals(0));
    });

    test('Status carries unknown host codes unchanged', () {
      expect(Status.ok.isOk, isTrue);
      expect(const Status(99).isOk, isFalse);
      expect(const Status(99).name, equals('other'));
    });
  });

  group('framing', () {
    test('length-prefix framing round-trips through the deframer', () {
      final framed = frameLengthPrefixed(bytes([1, 2, 3]));
      expect(bytesToHex(framed), equals('00000003010203'));
      final deframer = LengthPrefixedDeframer();
      // Feed it in two chunks to prove incremental assembly.
      deframer.push(Uint8List.sublistView(framed, 0, 2));
      expect(deframer.next(), isNull);
      deframer.push(Uint8List.sublistView(framed, 2));
      final frame = deframer.next();
      expect(frame, isNotNull);
      expect(bytesEqual(frame, bytes([1, 2, 3])), isTrue);
    });

    test('frame guard rejects oversized frames before allocating', () {
      expect(() => frameLengthPrefixed(bytes([0, 0]), max: 1),
          throwsA(isA<FrameTooLargeException>()));
      // A hostile length prefix is rejected before its body is buffered.
      final deframer = LengthPrefixedDeframer(max: 4);
      deframer.push(bytes([0x00, 0x00, 0x00, 0x10]));
      expect(deframer.next, throwsA(isA<FrameTooLargeException>()));
    });
  });

  group('RPC loopback', () {
    test('client and server exchange a request/response over a loopback pair',
        () {
      // Wire two loopbacks so the server reads what the client wrote and vice versa.
      final clientToServer = LoopbackFrameCarrier();
      final serverToClient = LoopbackFrameCarrier();
      final server =
          RpcServer(_PairedCarrier(send: serverToClient, recv: clientToServer));

      // A single-threaded loopback cannot interleave one synchronous call(), so
      // the exchange is driven step by step: the client's request is staged, the
      // server serves it, then the response is read back.
      final reqPayload = encode(const CborInt(7));
      final req = RpcRequest('Calc', 'square', reqPayload)..id = 1;
      clientToServer.pushInbound(req.encode());

      final served = server.serveOne((r) {
        expect(r.service, equals('Calc'));
        return Reply('SquareResult', encode(const CborInt(49)));
      });
      expect(served, isTrue);

      // The server replied via its carrier's sendFrame, which stages onto
      // serverToClient's inbound queue; read it back the same way the client would.
      final respFrame = serverToClient.recvFrame();
      expect(respFrame, isNotNull);
      final resp = RpcResponse.decode(respFrame!);
      expect(resp.status.isOk, isTrue);
      expect(resp.variant, equals('SquareResult'));
      expect(resp.id, equals(1));
    });

    test('non-zero transport status surfaces as a StatusException on call', () {
      final carrier = LoopbackFrameCarrier();
      final client = RpcClient(carrier, false);
      // Pre-load the server's error reply for the client to read.
      carrier.pushInbound(
          RpcResponse.transportError(Status.unknownServiceOrOp, 'no such op')
              .encode());
      expect(() => client.call('S', 'missing', Uint8List(0)),
          throwsA(isA<StatusException>()));
    });
  });

  group('events negotiation', () {
    test('hello negotiates the first mutually-supported profile in peer order',
        () {
      final hello = Hello([1], ['compact', 'verbose']);
      final result = hello.negotiate([Profile.verbose, Profile.compact]);
      expect(result, equals((1, Profile.compact)));
    });

    test('hello fails to negotiate an unknown version', () {
      final hello = Hello([99], ['verbose']);
      expect(hello.negotiate([Profile.verbose]), isNull);
    });
  });

  group('sequence tracking', () {
    test('classifies first / advance / gap / late / restart', () {
      final t = SeqTracker();
      expect(t.observe(0), equals(const SeqAdvanced(0))); // unsequenced
      expect(t.observe(5), equals(const SeqFirst()));
      expect(t.observe(6), equals(const SeqAdvanced(0)));
      expect(t.observe(9), equals(const SeqAdvanced(2))); // 7,8 missing
      expect(t.observe(8), equals(const SeqLateOrDuplicate()));
    });

    test('first-ever epoch is not a restart; a later epoch change is', () {
      final t = SeqTracker();
      // no-epoch -> first epoch is NOT a restart.
      expect(t.observe(3, epoch: 1), equals(const SeqFirst()));
      expect(t.observe(4, epoch: 1), equals(const SeqAdvanced(0)));
      // a genuine epoch change after a prior epoch is a restart.
      expect(t.observe(1, epoch: 2), equals(const SeqRestart()));
    });
  });
}

// A FrameCarrier that sends into one loopback and receives from another, so a
// client and server can be wired into a synchronous in-memory pair.
final class _PairedCarrier implements FrameCarrier {
  final LoopbackFrameCarrier send;
  final LoopbackFrameCarrier recv;
  _PairedCarrier({required this.send, required this.recv});

  @override
  void sendFrame(Uint8List frame) => send.pushInbound(frame);

  @override
  Uint8List? recvFrame() => recv.recvFrame();
}
