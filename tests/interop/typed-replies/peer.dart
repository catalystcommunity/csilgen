import 'services.gen.dart';
import 'types.gen.dart';

final class Carrier implements CsilTransport {
  @override
  List<int> call(String service, String op, List<int> request) {
    final req = Req.fromCbor(request);
    if (req.a == 'unknown') throw StateError('status 2');
    final (variant, payload) = encodeRpDecryptTokenReply(() {
      if (req.a == 'limit') {
        throw const RpErrorRefusal(RpError(code: RpErrorCode.applicationUserLimit, message: 'limit'));
      }
      return const Resp(b: 'ok');
    });
    if (variant == 'RpError') assert(payload[6] == 7);
    return resolveRpcReply(service, op, variant, payload);
  }
}

void main() {
  assert(RpErrorCode.accountRateLimited.wireNumber == 42);
  assert(RpErrorCode.fromNumber(42) == RpErrorCode.accountRateLimited);
  final client = RpClient(Carrier());
  assert(client.decryptToken(const Req(a: 'ok')).b == 'ok');
  try {
    client.decryptToken(const Req(a: 'limit'));
    throw Exception('missing refusal');
  } on RpErrorRefusal catch (e) {
    assert(e.value.code == RpErrorCode.applicationUserLimit);
  }
  try {
    client.decryptToken(const Req(a: 'unknown'));
    throw Exception('missing transport error');
  } on StateError catch (e) {
    assert(e.message == 'status 2');
  }
}
