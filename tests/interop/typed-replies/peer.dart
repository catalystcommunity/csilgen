import 'dart:typed_data';

import 'services.async.gen.dart';
import 'services.gen.dart';
import 'types.gen.dart';

// Answers like a server: the reply variant travels beside the payload, and the
// request text picks a forced variant to test a missing or unknown one.
(String?, Uint8List) answer(String service, String op, List<int> request) {
  if (service != 'Rp' || op != 'decrypt-token') throw StateError('route $service/$op');
  final req = Req.fromCbor(request);
  if (req.a == 'unknown') throw StateError('status 2');
  final (variant, payload) = encodeRpDecryptTokenReply(() {
    if (req.a == 'limit') {
      throw const RpErrorRefusal(RpError(code: RpErrorCode.applicationUserLimit, message: 'limit'));
    }
    return const Resp(b: 'ok');
  });
  if (variant == 'RpError') assert(payload[6] == 7);
  if (req.a == 'other') return ('Other', payload);
  if (req.a == 'none') return (null, payload);
  return (variant, payload);
}

final class Carrier implements CsilTransport {
  @override
  List<int> call(String service, String op, List<int> request) =>
      throw StateError('a multi-arm operation must use callReply');

  @override
  (String?, Uint8List) callReply(String service, String op, List<int> request) =>
      answer(service, op, request);
}

final class AsyncCarrier implements AsyncCsilTransport {
  @override
  Future<Uint8List> call(String service, String op, List<int> request) =>
      throw StateError('a multi-arm operation must use callReply');

  @override
  Future<(String?, Uint8List)> callReply(String service, String op, List<int> request) async =>
      answer(service, op, request);
}

Future<void> main() async {
  assert(RpErrorCode.accountRateLimited.wireNumber == 42);
  assert(RpErrorCode.fromNumber(42) == RpErrorCode.accountRateLimited);
  final client = RpClient(Carrier());
  final asyncClient = RpAsyncClient(AsyncCarrier());
  final calls = <String, Future<Resp> Function(Req)>{
    'sync': (req) async => client.decryptToken(req),
    'async': asyncClient.decryptToken,
  };
  for (final MapEntry(key: name, value: call) in calls.entries) {
    final ok = await call(const Req(a: 'ok'));
    if (ok.b != 'ok') throw StateError('$name: wrong success value');
    try {
      await call(const Req(a: 'limit'));
      throw StateError('$name: missing refusal');
    } on RpErrorRefusal catch (e) {
      if (e.value.code != RpErrorCode.applicationUserLimit) throw StateError('$name: wrong error');
    }
    for (final forced in ['other', 'none']) {
      try {
        await call(Req(a: forced));
        throw StateError('$name: accepted variant $forced');
      } on FormatException {
        // A missing or unknown variant is a decode error.
      }
    }
    try {
      await call(const Req(a: 'unknown'));
      throw StateError('$name: missing transport error');
    } on StateError catch (e) {
      if (e.message != 'status 2') rethrow;
    }
  }
  print('dart: typed replies pass');
}
