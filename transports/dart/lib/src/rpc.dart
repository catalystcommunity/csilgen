// CSIL-RPC transport — request/response/push envelopes — see `csil-rpc-transport.md`.

import 'dart:typed_data';

import 'carrier.dart';
import 'cbor.dart';
// The codec's `encode`/`decode` are prefixed so they never collide with an
// envelope class's own `encode()`/`decode` members; the `Cbor*` value types stay
// unprefixed for terseness.
import 'cbor.dart' as cbor show encode, decode;
import 'conventions.dart';

/// A CSIL-RPC request (client → server). `payload` is the opaque CBOR(request
/// type) bytes; the transport wraps it in tag 24 on the wire and never inspects it.
final class RpcRequest {
  final String service;
  final String op;
  int? id;
  String? auth;
  Uint8List payload;

  RpcRequest(this.service, this.op, this.payload);

  Uint8List encode() {
    final entries = <(String, Cbor)>[
      ('v', CborInt(version)),
      ('service', CborText(service)),
      ('op', CborText(op)),
      ('payload', tag24(payload)),
    ];
    if (id != null) entries.add(('id', CborInt(id!)));
    if (auth != null) entries.add(('auth', CborText(auth!)));
    return cbor.encode(canonMap(entries));
  }

  static RpcRequest decode(Uint8List bytes) {
    final v = cbor.decode(bytes);
    checkVersion(getUint(v, 'v'));
    final payloadField = mapGet(v, 'payload');
    if (payloadField == null) {
      throw const MalformedException("missing 'payload'");
    }
    final req = RpcRequest(
        getText(v, 'service'), getText(v, 'op'), untag24(payloadField));
    req.id = getUintOpt(v, 'id');
    req.auth = getTextOpt(v, 'auth');
    return req;
  }

  @override
  bool operator ==(Object other) =>
      other is RpcRequest &&
      other.service == service &&
      other.op == op &&
      other.id == id &&
      other.auth == auth &&
      bytesEqual(other.payload, payload);

  @override
  int get hashCode => Object.hash(service, op, id, auth, payload.length);
}

/// A CSIL-RPC response (server → client). `status` is the transport outcome;
/// `variant` names the declared output-choice arm `payload` decodes to when
/// `status` is Ok. `payload` is empty (but present) when `status` is non-zero.
final class RpcResponse {
  int? id;
  Status status;
  String? variant;
  String? error;
  Uint8List payload;

  RpcResponse(this.status, this.payload);

  /// A successful (status Ok) typed reply.
  static RpcResponse okReply(String variant, Uint8List payload) {
    final r = RpcResponse(Status.ok, payload);
    r.variant = variant;
    return r;
  }

  /// A transport-level failure (no typed payload).
  static RpcResponse transportError(Status status, String message) {
    final r = RpcResponse(status, Uint8List(0));
    r.error = message;
    return r;
  }

  Uint8List encode() {
    final entries = <(String, Cbor)>[
      ('v', CborInt(version)),
      ('status', CborInt(status.code)),
      ('payload', tag24(payload)),
    ];
    if (id != null) entries.add(('id', CborInt(id!)));
    if (variant != null) entries.add(('variant', CborText(variant!)));
    if (error != null) entries.add(('error', CborText(error!)));
    return cbor.encode(canonMap(entries));
  }

  static RpcResponse decode(Uint8List bytes) {
    final v = cbor.decode(bytes);
    checkVersion(getUint(v, 'v'));
    // payload is present but may be an empty byte string on error.
    final payloadField = mapGet(v, 'payload');
    final payload = payloadField != null ? untag24(payloadField) : Uint8List(0);
    final r = RpcResponse(Status(getInt(v, 'status')), payload);
    r.id = getUintOpt(v, 'id');
    r.variant = getTextOpt(v, 'variant');
    r.error = getTextOpt(v, 'error');
    return r;
  }

  /// Throw [StatusException] for a non-ok response so callers surface transport
  /// failures distinctly from application errors (which ride as status-0 variants).
  RpcResponse intoTransportError() {
    if (status.isOk) return this;
    throw StatusException(status, error);
  }

  @override
  bool operator ==(Object other) =>
      other is RpcResponse &&
      other.id == id &&
      other.status == status &&
      other.variant == variant &&
      other.error == error &&
      bytesEqual(other.payload, payload);

  @override
  int get hashCode => Object.hash(id, status, variant, error, payload.length);
}

/// A CSIL-RPC server push (server → client) for `<-` operations. No id (not a
/// reply) and no status (cannot fail in the request/response sense).
final class RpcPush {
  final String service;
  final String event;
  Uint8List payload;

  RpcPush(this.service, this.event, this.payload);

  Uint8List encode() => cbor.encode(canonMap(<(String, Cbor)>[
        ('v', CborInt(version)),
        ('service', CborText(service)),
        ('event', CborText(event)),
        ('payload', tag24(payload)),
      ]));

  static RpcPush decode(Uint8List bytes) {
    final v = cbor.decode(bytes);
    checkVersion(getUint(v, 'v'));
    final payloadField = mapGet(v, 'payload');
    if (payloadField == null) {
      throw const MalformedException("missing 'payload'");
    }
    return RpcPush(
        getText(v, 'service'), getText(v, 'event'), untag24(payloadField));
  }

  @override
  bool operator ==(Object other) =>
      other is RpcPush &&
      other.service == service &&
      other.event == event &&
      bytesEqual(other.payload, payload);

  @override
  int get hashCode => Object.hash(service, event, payload.length);
}

/// The outcome a server handler returns for one request: a typed reply (variant
/// name + encoded payload) on success, or a transport status on failure. A
/// `sealed` hierarchy so a host pattern-matches it exhaustively.
sealed class HandlerOutcome {
  const HandlerOutcome();
}

final class Reply extends HandlerOutcome {
  final String variant;
  final Uint8List payload;
  const Reply(this.variant, this.payload);
}

final class TransportOutcome extends HandlerOutcome {
  final Status status;
  final String message;
  const TransportOutcome(this.status, this.message);
}

/// A CSIL-RPC client over a frame carrier. The carrier is injected (bring your
/// own); the client owns the envelope and a per-connection monotonic id. The codec
/// is synchronous — a host on an async socket buffers at the carrier seam.
final class RpcClient {
  int _nextId = 1;
  final FrameCarrier carrier;
  final bool multiplexed;

  /// `multiplexed` true assigns a correlation id to every request (required on
  /// WS / pipelined streams); false omits it (strictly one-in-flight carriers).
  RpcClient(this.carrier, this.multiplexed);

  /// Invoke `service`/`op` with an encoded request payload, returning the decoded
  /// response. A non-zero transport status is thrown as [StatusException].
  RpcResponse call(String service, String op, Uint8List payload,
      {String? auth}) {
    final req = RpcRequest(service, op, payload);
    req.auth = auth;
    if (multiplexed) {
      req.id = _nextId;
      _nextId += 1;
    }
    carrier.sendFrame(req.encode());
    final frame = carrier.recvFrame();
    if (frame == null) {
      throw CarrierException('connection closed before response');
    }
    return RpcResponse.decode(frame).intoTransportError();
  }
}

/// A CSIL-RPC server over a frame carrier. The host supplies a handler mapping a
/// request to an outcome; the generated router is the natural implementation.
final class RpcServer {
  final FrameCarrier carrier;

  RpcServer(this.carrier);

  /// Read one request, dispatch it through `handler`, and write the response.
  /// Returns `false` at a clean end of stream.
  bool serveOne(HandlerOutcome Function(RpcRequest req) handler) {
    final frame = carrier.recvFrame();
    if (frame == null) return false;

    RpcResponse resp;
    try {
      final req = RpcRequest.decode(frame);
      final outcome = handler(req);
      switch (outcome) {
        case Reply(variant: final variant, payload: final payload):
          resp = RpcResponse.okReply(variant, payload)..id = req.id;
        case TransportOutcome(status: final status, message: final message):
          resp = RpcResponse.transportError(status, message)..id = req.id;
      }
    } on TransportException catch (e) {
      resp = RpcResponse.transportError(Status.malformedEnvelope, e.message);
    }
    carrier.sendFrame(resp.encode());
    return true;
  }
}
