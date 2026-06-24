// Verify the Dart reference library against the checked-in conformance vectors.
//
// This both guards the Dart encoders/decoders against drift and demonstrates the
// contract every reference library follows: reconstruct each vector's envelope
// from its language-neutral `input`, assert encode → `hex`, and assert
// decode(`hex`) → the same envelope. Ported from transports/go/conformance_test.go
// and transports/rust/tests/conformance.rs.

import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:csilgen_transport/csilgen_transport.dart';
import 'package:test/test.dart';

List<Map<String, dynamic>> loadVectors(String name) {
  // `dart test` runs with the package root as the working directory, so the
  // shared vectors sit one level up under conformance/.
  final file = File('../conformance/$name');
  final doc = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
  return (doc['vectors'] as List).cast<Map<String, dynamic>>();
}

Map<String, dynamic> input(Map<String, dynamic> vec) =>
    vec['input'] as Map<String, dynamic>;

String? optStr(Map<String, dynamic> m, String k) => m[k] as String?;

int? optInt(Map<String, dynamic> m, String k) => m[k] as int?;

String reqStr(Map<String, dynamic> m, String k) => m[k] as String;

int reqInt(Map<String, dynamic> m, String k) => m[k] as int;

void main() {
  test('RPC vectors', () {
    for (final vec in loadVectors('rpc.json')) {
      final inp = input(vec);
      final name = vec['name'];
      final wantHex = vec['hex'] as String;
      Uint8List actual;

      switch (reqStr(inp, 'kind')) {
        case 'request':
          final req = RpcRequest(reqStr(inp, 'service'), reqStr(inp, 'op'),
              hexToBytes(reqStr(inp, 'payload_hex')));
          req.id = optInt(inp, 'id');
          req.auth = optStr(inp, 'auth');
          actual = req.encode();
          expect(RpcRequest.decode(actual), equals(req),
              reason: '$name decode');
        case 'response':
          final resp = RpcResponse(Status(reqInt(inp, 'status')),
              hexToBytes(reqStr(inp, 'payload_hex')));
          resp.id = optInt(inp, 'id');
          resp.variant = optStr(inp, 'variant');
          resp.error = optStr(inp, 'error');
          actual = resp.encode();
          expect(RpcResponse.decode(actual), equals(resp),
              reason: '$name decode');
        case 'push':
          final push = RpcPush(reqStr(inp, 'service'), reqStr(inp, 'event'),
              hexToBytes(reqStr(inp, 'payload_hex')));
          actual = push.encode();
          expect(RpcPush.decode(actual), equals(push), reason: '$name decode');
        default:
          fail('unknown rpc kind for $name');
      }
      expect(bytesToHex(actual), equals(wantHex), reason: '$name encode');
    }
  });

  test('Events vectors', () {
    for (final vec in loadVectors('events.json')) {
      final inp = input(vec);
      final name = vec['name'];
      final wantHex = vec['hex'] as String;

      // Control payloads are keyed by "control"; application events by "profile".
      final control = optStr(inp, 'control');
      if (control != null) {
        Uint8List bytes;
        switch (control) {
          case 'hello':
            final versions = (inp['versions'] as List).cast<int>();
            final profiles = (inp['profiles'] as List).cast<String>();
            bytes = Hello(versions, profiles,
                    service: optStr(inp, 'service'), auth: optStr(inp, 'auth'))
                .encode();
          case 'hello_ack':
            bytes = HelloAck(reqInt(inp, 'v'), reqStr(inp, 'profile'),
                    session: optStr(inp, 'session'))
                .encode();
          case 'ping':
            bytes =
                Heartbeat(reqInt(inp, 'nonce'), at: optInt(inp, 'at')).encode();
          case 'close':
            bytes = Close(Status(reqInt(inp, 'status')),
                    reason: optStr(inp, 'reason'))
                .encode();
          default:
            fail('unknown control $control');
        }
        expect(bytesToHex(bytes), equals(wantHex), reason: '$name encode');
        continue;
      }

      final profile = parseProfile(reqStr(inp, 'profile'))!;
      final payload = hexToBytes(reqStr(inp, 'payload_hex'));
      Event ev;
      switch (profile) {
        case Profile.verbose:
          ev = Event.verbose(
              optStr(inp, 'service'), reqStr(inp, 'event'), payload);
          ev.id = optInt(inp, 'id');
        case Profile.compact:
          ev = Event.compact(
              reqInt(inp, 'service_ord'), reqInt(inp, 'op_ord'), payload);
          ev.id = optInt(inp, 'id');
      }
      final bytes = ev.encode(profile);
      expect(bytesToHex(bytes), equals(wantHex), reason: '$name encode');
      expect(Event.decode(bytes, profile), equals(ev), reason: '$name decode');
    }
  });

  test('Datagram vectors', () {
    for (final vec in loadVectors('datagrams.json')) {
      final inp = input(vec);
      final name = vec['name'];
      final wantHex = vec['hex'] as String;

      switch (reqStr(inp, 'profile')) {
        case 'cbor-array':
          final d = Datagram(reqInt(inp, 'op_ord'), reqInt(inp, 'seq'),
              hexToBytes(reqStr(inp, 'payload_hex')));
          final bytes = d.encode();
          expect(bytesToHex(bytes), equals(wantHex), reason: '$name encode');
          expect(Datagram.decode(bytes), equals(d), reason: '$name decode');
        case 'compact-header':
          final d = CompactDatagram(reqInt(inp, 'op_ord'), reqInt(inp, 'seq'),
              hexToBytes(reqStr(inp, 'body_hex')));
          final epoch = optInt(inp, 'epoch');
          if (epoch != null) d.withEpoch(epoch);
          final bytes = d.encode();
          expect(bytesToHex(bytes), equals(wantHex), reason: '$name encode');
          expect(CompactDatagram.decode(bytes), equals(d),
              reason: '$name decode');
        default:
          fail('unknown datagram profile for $name');
      }
    }
  });
}
