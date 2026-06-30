// Dart interop harness — mirrors the Rust reference (tests/interop/harness/rust).
//
// Runs as a server (binds a loopback TCP/UDP port and serves one transport) or a
// client (connects, runs the case battery for one transport, prints one JSON
// results object). The on-wire behavior and case names match the Rust reference so
// a Dart client works against a Rust server and vice versa. See
// tests/interop/README.md.
//
// Transport mapping (carrier-independent codec, same as the reference):
//   CSIL-RPC / CSIL-Events → loopback TCP, the lib's 4-byte length-prefix framing.
//   CSIL-Datagrams         → loopback UDP (RawDatagramSocket), the lib's datagram
//                            envelope; the client uses an ephemeral source port and
//                            the server replies to the recvfrom source.
//
// The CSIL transport seam is synchronous (FrameCarrier), but dart:io sockets are
// Future/Stream-based. The two sides bridge differently because of where the seam
// blocks:
//   - The client drives the lib's synchronous RpcClient.call / Event flows, which
//     send-then-recv in one call, so it needs a *blocking* socket: dart:io's
//     RawSynchronousSocket (the only synchronous socket in the stdlib).
//   - The server always reads a frame before it writes one, so it uses an async
//     ServerSocket and feeds each accepted Socket through an inbox-backed
//     FrameCarrier: the choreography awaits a complete inbound frame, then runs the
//     synchronous lib/dispatch step (which drains that frame and writes the reply).

import 'dart:async';
import 'dart:collection';
import 'dart:io';
import 'dart:typed_data';

import 'package:csilgen_transport/csilgen_transport.dart';
import 'package:interop_api/interop_api.dart';

const String service = 'interop';

// ---------------------------------------------------------------------------
// Fixed language-neutral test vectors (see README "Fixed test vectors").
// ---------------------------------------------------------------------------

Scalars scalarsOk() => Scalars(
      i: -42,
      u: 42,
      n: -7,
      f: 3.5,
      t: 'héllo 世界',
      raw: Uint8List.fromList([0x01, 0x02, 0xf0, 0xff]),
      flag: true,
      when_: DateTime.parse('2026-06-29T12:34:56Z').toUtc(),
      amount: CsilDecimal.parse('123.45'),
    );

Collections collectionsOk() => Collections(
      names: ['a', 'b'],
      atLeastOne: [1, 2, 3],
      bounded: [10, 20],
      exact3: [7, 8, 9],
      scores: {'x': 1, 'y': 2},
      color: 'green',
      tone: 'blue',
      prio: 2,
      who: IdOrNameVariant0(4242),
      pair: ('p', 5),
      triple: ('t', 9, true),
      extra: {'k': 'v'},
    );

Constrained constrainedOk() => Constrained(
      code: 'PRD-AB12CD',
      qty: 10,
      rate: CsilDecimal.parse('0.25'),
      password: 'hunter2hunter2',
      tags: ['one', 'two'],
    );

Constrained constrainedBad() => Constrained(
      code: 'bad',
      qty: 0,
      rate: CsilDecimal.parse('9.9'),
      password: 'x',
      tags: [],
    );

Nested nestedOk() =>
    Nested(inner: scalarsOk(), maybe: constrainedOk(), many: [scalarsOk()]);

// Structural equality over the dynamic CBOR tree a value lowers to. Comparing the
// `toCborValue()` trees sidesteps Dart's reference equality for List/Map/union and
// is order-independent for maps — exactly the round-trip equality the matrix needs.
bool treeEqual(Object? a, Object? b) {
  if (identical(a, b)) return true;
  if (a is CsilDecimal && b is CsilDecimal) return a.compareTo(b) == 0;
  if (a is DateTime && b is DateTime) {
    return a.toUtc().isAtSameMomentAs(b.toUtc());
  }
  if (a is Uint8List && b is Uint8List) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (a[i] != b[i]) return false;
    }
    return true;
  }
  if (a is List && b is List) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (!treeEqual(a[i], b[i])) return false;
    }
    return true;
  }
  if (a is Map && b is Map) {
    if (a.length != b.length) return false;
    for (final k in a.keys) {
      if (!b.containsKey(k) || !treeEqual(a[k], b[k])) return false;
    }
    return true;
  }
  return a == b;
}

bool scalarsEq(Scalars a, Scalars b) => treeEqual(a.toCborValue(), b.toCborValue());
bool collectionsEq(Collections a, Collections b) =>
    treeEqual(a.toCborValue(), b.toCborValue());
bool constrainedEq(Constrained a, Constrained b) =>
    treeEqual(a.toCborValue(), b.toCborValue());
bool nestedEq(Nested a, Nested b) => treeEqual(a.toCborValue(), b.toCborValue());

// ---------------------------------------------------------------------------
// Result accumulation + JSON output.
// ---------------------------------------------------------------------------

class Cases {
  final String lang = 'dart';
  final String transport;
  final List<(String, bool, String)> out = [];
  Cases(this.transport);

  void pass(String name) => out.add((name, true, ''));
  void fail(String name, String detail) => out.add((name, false, detail));
  void check(String name, bool cond, String detail) =>
      cond ? pass(name) : fail(name, detail);

  void emit() {
    final sb = StringBuffer();
    sb.write('{"lang":"$lang","transport":"$transport","cases":[');
    for (var i = 0; i < out.length; i++) {
      if (i > 0) sb.write(',');
      final (name, ok, detail) = out[i];
      sb.write('{"name":${_jsonStr(name)},"ok":$ok,"detail":${_jsonStr(detail)}}');
    }
    sb.write(']}\n');
    stdout.write(sb.toString());
  }
}

String _jsonStr(String s) {
  final sb = StringBuffer('"');
  for (final rune in s.runes) {
    switch (rune) {
      case 0x22:
        sb.write('\\"');
      case 0x5c:
        sb.write('\\\\');
      case 0x0a:
        sb.write('\\n');
      case 0x09:
        sb.write('\\t');
      case 0x0d:
        sb.write('\\r');
      default:
        if (rune < 0x20) {
          sb.write('\\u${rune.toRadixString(16).padLeft(4, '0')}');
        } else {
          sb.writeCharCode(rune);
        }
    }
  }
  sb.write('"');
  return sb.toString();
}

// ---------------------------------------------------------------------------
// Server-side handler implementing the generated interface (also drives the
// generated channel router on the unknown-method path, as the reference does).
// ---------------------------------------------------------------------------

final class Handlers implements InteropHandler {
  @override
  Scalars echoScalars(Scalars request) => request;
  @override
  Collections echoCollections(Collections request) => request;
  @override
  EchoNestedResult echoNested(Nested request) =>
      EchoNestedResult(ok: true, echo: request);
  @override
  Constrained validateConstrained(Constrained request) {
    request.validate();
    return request;
  }
  @override
  void duplex(Scalars message) {}
}

final class TreeCodec implements CsilCodec {
  @override
  List<int> encode(Object? value) => CsilCbor.encodeValue(value);
  @override
  Object? decode(List<int> data) => CsilCbor.decode(data);
}

// ---------------------------------------------------------------------------
// Carriers — loopback TCP + the lib's 4-byte length framing.
// ---------------------------------------------------------------------------

/// Client carrier over a *blocking* synchronous TCP socket. `recvFrame` blocks for
/// the length prefix and then the body, exactly the seam the lib's synchronous
/// RpcClient.call / Event flows expect.
final class SyncStreamFrameCarrier implements FrameCarrier {
  final RawSynchronousSocket sock;
  SyncStreamFrameCarrier(this.sock);

  @override
  void sendFrame(Uint8List frame) => sock.writeFromSync(frameLengthPrefixed(frame));

  @override
  Uint8List? recvFrame() {
    final header = _recvExact(4);
    if (header == null) return null;
    final len =
        (header[0] << 24) | (header[1] << 16) | (header[2] << 8) | header[3];
    if (len == 0) return Uint8List(0);
    return _recvExact(len);
  }

  /// Read exactly `n` bytes, or `null` at EOF (stream framing relies on this).
  Uint8List? _recvExact(int n) {
    final out = Uint8List(n);
    var got = 0;
    while (got < n) {
      final chunk = sock.readSync(n - got);
      if (chunk == null || chunk.isEmpty) return null; // EOF
      out.setRange(got, got + chunk.length, chunk);
      got += chunk.length;
    }
    return out;
  }
}

/// Server carrier over an async dart:io [Socket]. Inbound chunks are deframed into
/// a queue as they arrive; [awaitFrame] lets the (otherwise synchronous) server
/// choreography wait for a complete frame before invoking the sync recv path.
final class InboxFrameCarrier implements FrameCarrier {
  final Socket socket;
  final LengthPrefixedDeframer _deframer = LengthPrefixedDeframer();
  final Queue<Uint8List> _frames = Queue<Uint8List>();
  bool _closed = false;
  Completer<void>? _waiter;

  InboxFrameCarrier(this.socket) {
    socket.listen(
      (chunk) {
        _deframer.push(chunk);
        while (true) {
          final f = _deframer.next();
          if (f == null) break;
          _frames.add(f);
        }
        _wake();
      },
      onDone: () {
        _closed = true;
        _wake();
      },
      onError: (_) {
        _closed = true;
        _wake();
      },
      cancelOnError: true,
    );
  }

  void _wake() {
    final w = _waiter;
    if (w != null && !w.isCompleted) {
      _waiter = null;
      w.complete();
    }
  }

  /// Resolves once at least one frame is buffered, or the stream has closed.
  Future<void> awaitFrame() async {
    while (_frames.isEmpty && !_closed) {
      _waiter ??= Completer<void>();
      await _waiter!.future;
    }
  }

  @override
  void sendFrame(Uint8List frame) => socket.add(frameLengthPrefixed(frame));

  @override
  Uint8List? recvFrame() => _frames.isEmpty ? null : _frames.removeFirst();
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

/// Raised by the carrier when a status-0 reply carries the `ServiceError` arm, so
/// the typed error surfaces as the language error path (not a transport error).
final class ServiceErrorException implements Exception {
  final ServiceError error;
  ServiceErrorException(this.error);
  @override
  String toString() => 'service error ${error.code}: ${error.message}';
}

final class RpcServiceTransport implements CsilTransport {
  final RpcClient client;
  RpcServiceTransport(this.client);

  @override
  List<int> call(String svc, String op, List<int> request) {
    final resp = client.call(svc, op, Uint8List.fromList(request));
    // A status-0 reply whose variant is `ServiceError` is a typed application
    // error; the carrier maps it to the language error path (the transport
    // convention). Otherwise hand the success payload to the typed client.
    if (resp.variant == 'ServiceError') {
      throw ServiceErrorException(ServiceError.fromCbor(resp.payload));
    }
    return resp.payload;
  }
}

HandlerOutcome rpcDispatch(Handlers handlers, RpcRequest req) {
  try {
    switch (req.op) {
      case 'EchoScalars':
        return Reply('Scalars', handlers.echoScalars(Scalars.fromCbor(req.payload)).toCbor());
      case 'EchoCollections':
        return Reply('Collections',
            handlers.echoCollections(Collections.fromCbor(req.payload)).toCbor());
      case 'EchoNested':
        return Reply('EchoNestedResult',
            handlers.echoNested(Nested.fromCbor(req.payload)).toCbor());
      case 'ValidateConstrained':
        final input = Constrained.fromCbor(req.payload);
        try {
          // The typed error arm rides as a status-0 `ServiceError` variant.
          return Reply('Constrained', handlers.validateConstrained(input).toCbor());
        } on ArgumentError catch (e) {
          return Reply('ServiceError',
              ServiceError(code: 422, message: '${e.message}').toCbor());
        }
      default:
        return TransportOutcome(Status.unknownServiceOrOp, 'unknown op ${req.op}');
    }
  } on TransportException catch (e) {
    return TransportOutcome(Status.malformedEnvelope, e.message);
  } catch (e) {
    return TransportOutcome(Status.malformedEnvelope, '$e');
  }
}

Future<void> rpcServeConnection(InboxFrameCarrier carrier) async {
  final handlers = Handlers();
  final server = RpcServer(carrier);
  while (true) {
    await carrier.awaitFrame();
    // serveOne drains the buffered request and writes its reply; a clean EOF (the
    // awaitFrame above returning with no buffered frame) ends the connection.
    if (!server.serveOne((req) => rpcDispatch(handlers, req))) break;
  }
}

Cases rpcClient(SyncStreamFrameCarrier carrier) {
  final cases = Cases('rpc');
  final transport = RpcServiceTransport(RpcClient(carrier, false));
  final client = InteropClient(transport);

  try {
    final r = client.echoScalars(scalarsOk());
    cases.check('echo-scalars/success', scalarsEq(r, scalarsOk()), 'mismatch');
  } catch (e) {
    cases.fail('echo-scalars/success', '$e');
  }
  try {
    final r = client.echoCollections(collectionsOk());
    cases.check(
        'echo-collections/success', collectionsEq(r, collectionsOk()), 'mismatch');
  } catch (e) {
    cases.fail('echo-collections/success', '$e');
  }
  try {
    final r = client.echoNested(nestedOk());
    cases.check('echo-nested/success', r.ok && nestedEq(r.echo, nestedOk()),
        'mismatch');
  } catch (e) {
    cases.fail('echo-nested/success', '$e');
  }
  try {
    final r = client.validateConstrained(constrainedOk());
    cases.check('validate-constrained/success',
        constrainedEq(r, constrainedOk()), 'mismatch');
  } catch (e) {
    cases.fail('validate-constrained/success', '$e');
  }
  try {
    client.validateConstrained(constrainedBad());
    cases.fail('validate-constrained/failure', 'server accepted invalid input');
  } on ServiceErrorException {
    // The typed error arm must surface as a service error.
    cases.pass('validate-constrained/failure');
  } catch (e) {
    cases.fail('validate-constrained/failure', 'expected service error, got $e');
  }
  return cases;
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

const int ticks = 3;

void sendEvent(FrameCarrier carrier, Event ev) =>
    carrier.sendFrame(ev.encode(Profile.verbose));

Future<void> eventsServeConnection(InboxFrameCarrier carrier) async {
  final handlers = Handlers();
  final codec = TreeCodec();

  // Handshake: read $hello, reply $hello-ack (verbose).
  await carrier.awaitFrame();
  final helloFrame = carrier.recvFrame();
  if (helloFrame == null) return;
  final helloEv = Event.decode(helloFrame, Profile.verbose);
  if (helloEv.event != Control.helloName) return;
  Hello.decode(helloEv.payload);
  sendEvent(carrier,
      Event.verbose(null, Control.helloAckName, HelloAck(1, 'verbose').encode()));

  // Push N ticks (on-tick / server push).
  for (var seq = 0; seq < ticks; seq++) {
    sendEvent(carrier, Event.verbose(service, 'OnTick', Tick(seq: seq).toCbor()));
  }

  // React loop.
  while (true) {
    await carrier.awaitFrame();
    final frame = carrier.recvFrame();
    if (frame == null) break;
    Event ev;
    try {
      ev = Event.decode(frame, Profile.verbose);
    } catch (_) {
      break;
    }
    final method = ev.event ?? '';
    if (method == Control.pingName) {
      sendEvent(carrier, Event.verbose(null, Control.pongName, ev.payload));
    } else if (method == Control.closeName) {
      break;
    } else if (method == 'Duplex') {
      try {
        final s = Scalars.fromCbor(ev.payload);
        handlers.duplex(s);
        sendEvent(carrier, Event.verbose(service, 'Duplex', s.toCbor()));
      } catch (_) {
        // a malformed duplex frame is ignored, as in the reference
      }
    } else {
      // Exercise the generated router's unknown-method path, then signal $error.
      try {
        routeInteropChannel(handlers, codec, method, ev.payload);
      } catch (_) {
        // expected: the router rejects an unknown channel op
      }
      sendEvent(carrier, Event.verbose(null, Control.errorName, Uint8List(0)));
    }
  }
}

Cases eventsClient(SyncStreamFrameCarrier carrier) {
  final cases = Cases('events');

  // Handshake.
  sendEvent(
      carrier,
      Event.verbose(null, Control.helloName,
          Hello([1], ['verbose'], service: service).encode()));
  final ackFrame = carrier.recvFrame();
  var ackOk = false;
  if (ackFrame != null) {
    try {
      ackOk = Event.decode(ackFrame, Profile.verbose).event == Control.helloAckName;
    } catch (_) {
      ackOk = false;
    }
  }
  cases.check('events/handshake', ackOk, 'no \$hello-ack');

  // on-tick: expect `ticks` pushes with seq 0..ticks.
  var tickOk = true;
  var detail = '';
  for (var expect = 0; expect < ticks; expect++) {
    final frame = carrier.recvFrame();
    if (frame == null) {
      tickOk = false;
      detail = 'stream closed during ticks';
      break;
    }
    try {
      final ev = Event.decode(frame, Profile.verbose);
      if (ev.event != 'OnTick') {
        tickOk = false;
        detail = 'expected OnTick got ${ev.event}';
        break;
      }
      final t = Tick.fromCbor(ev.payload);
      if (t.seq != expect) {
        tickOk = false;
        detail = 'tick seq ${t.seq} != $expect';
        break;
      }
    } catch (e) {
      tickOk = false;
      detail = 'decode: $e';
      break;
    }
  }
  cases.check('on-tick/success', tickOk, detail);

  // duplex: send Scalars, expect echo.
  sendEvent(carrier, Event.verbose(service, 'Duplex', scalarsOk().toCbor()));
  final dupFrame = carrier.recvFrame();
  if (dupFrame == null) {
    cases.fail('duplex/success', 'no echo');
  } else {
    try {
      final ev = Event.decode(dupFrame, Profile.verbose);
      if (ev.event == 'Duplex') {
        final s = Scalars.fromCbor(ev.payload);
        cases.check('duplex/success', scalarsEq(s, scalarsOk()), 'echo mismatch');
      } else {
        cases.fail('duplex/success', 'got ${ev.event}');
      }
    } catch (e) {
      cases.fail('duplex/success', '$e');
    }
  }

  // unknown-method: send a bogus channel method, expect $error.
  sendEvent(carrier, Event.verbose(service, 'Bogus', Uint8List(0)));
  final errFrame = carrier.recvFrame();
  if (errFrame == null) {
    cases.fail('unknown-method/failure', 'no \$error');
  } else {
    var isErr = false;
    try {
      isErr = Event.decode(errFrame, Profile.verbose).event == Control.errorName;
    } catch (_) {
      isErr = false;
    }
    cases.check('unknown-method/failure', isErr, 'expected \$error');
  }

  sendEvent(carrier, Event.verbose(null, Control.closeName, Uint8List(0)));
  return cases;
}

// ---------------------------------------------------------------------------
// Datagrams
// ---------------------------------------------------------------------------

const int opEchoScalars = 0;
const int opEchoCollections = 1;
const int opErrorSentinel = 255;

/// Single-subscription pump over a [RawDatagramSocket]: received payloads queue up
/// and [next] awaits the next one (with a timeout for the dropped-datagram case).
final class DatagramInbox {
  final RawDatagramSocket sock;
  final Queue<Uint8List> _data = Queue<Uint8List>();
  Completer<void>? _waiter;

  DatagramInbox(this.sock) {
    sock.listen((event) {
      if (event == RawSocketEvent.read) {
        final dg = sock.receive();
        if (dg != null) {
          _data.add(dg.data);
          _wake();
        }
      }
    });
  }

  void _wake() {
    final w = _waiter;
    if (w != null && !w.isCompleted) {
      _waiter = null;
      w.complete();
    }
  }

  Future<Uint8List?> next(Duration timeout) async {
    if (_data.isNotEmpty) return _data.removeFirst();
    final waiter = _waiter ??= Completer<void>();
    try {
      await waiter.future.timeout(timeout);
    } on TimeoutException {
      // Abandon this waiter; a later arrival just queues for the next call.
      if (identical(_waiter, waiter)) _waiter = null;
      return null;
    }
    return _data.isEmpty ? null : _data.removeFirst();
  }
}

Future<void> datagramServer(RawDatagramSocket sock) async {
  await for (final event in sock) {
    if (event != RawSocketEvent.read) continue;
    final incoming = sock.receive();
    if (incoming == null) continue;
    Datagram reply;
    try {
      final dg = Datagram.decode(incoming.data);
      if (dg.opOrd == opEchoScalars) {
        final s = Scalars.fromCbor(dg.payload);
        reply = Datagram(opEchoScalars, dg.seq, s.toCbor());
      } else if (dg.opOrd == opEchoCollections) {
        final c = Collections.fromCbor(dg.payload);
        reply = Datagram(opEchoCollections, dg.seq, c.toCbor());
      } else {
        reply = Datagram(opErrorSentinel, dg.seq, Uint8List(0));
      }
    } catch (_) {
      reply = Datagram(opErrorSentinel, 0, Uint8List(0));
    }
    try {
      sock.send(reply.encode(), incoming.address, incoming.port);
    } catch (_) {
      // drop on send error
    }
  }
}

Future<Cases> datagramClient(RawDatagramSocket sock, int port) async {
  final cases = Cases('datagrams');
  final inbox = DatagramInbox(sock);
  final addr = InternetAddress.loopbackIPv4;

  Future<Datagram> roundtrip(int op, int seq, Uint8List payload) async {
    sock.send(Datagram(op, seq, payload).encode(), addr, port);
    final data = await inbox.next(const Duration(seconds: 3));
    if (data == null) throw StateError('recv timed out');
    return Datagram.decode(data);
  }

  try {
    final d = await roundtrip(opEchoScalars, 1, scalarsOk().toCbor());
    if (d.opOrd != opEchoScalars) {
      cases.fail('echo-scalars/success', 'op_ord ${d.opOrd}');
    } else {
      cases.check('echo-scalars/success',
          scalarsEq(Scalars.fromCbor(d.payload), scalarsOk()), 'payload mismatch');
    }
  } catch (e) {
    cases.fail('echo-scalars/success', '$e');
  }
  try {
    final d = await roundtrip(opEchoCollections, 2, collectionsOk().toCbor());
    if (d.opOrd != opEchoCollections) {
      cases.fail('echo-collections/success', 'op_ord ${d.opOrd}');
    } else {
      cases.check(
          'echo-collections/success',
          collectionsEq(Collections.fromCbor(d.payload), collectionsOk()),
          'payload mismatch');
    }
  } catch (e) {
    cases.fail('echo-collections/success', '$e');
  }
  try {
    final d = await roundtrip(99, 3, Uint8List(0));
    cases.check('bad-op-ord/failure', d.opOrd == opErrorSentinel,
        'expected error sentinel, got op_ord ${d.opOrd}');
  } catch (e) {
    cases.fail('bad-op-ord/failure', '$e');
  }
  return cases;
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

Future<void> main(List<String> args) async {
  if (args.length < 3) {
    stderr.writeln('usage: main.dart <server|client> <rpc|events|datagrams> <port>');
    exitCode = 2;
    return;
  }
  final mode = args[0];
  final transport = args[1];
  final port = int.parse(args[2]);
  final addr = InternetAddress.loopbackIPv4;

  if (mode == 'server' && transport == 'datagrams') {
    final sock = await RawDatagramSocket.bind(addr, port);
    await announceReady();
    await datagramServer(sock); // blocks
    return;
  }
  if (mode == 'server') {
    final server = await bindRetry(port);
    await announceReady();
    await for (final socket in server) {
      final carrier = InboxFrameCarrier(socket);
      try {
        if (transport == 'rpc') {
          await rpcServeConnection(carrier);
        } else if (transport == 'events') {
          await eventsServeConnection(carrier);
        } else {
          stderr.writeln('bad transport');
          return;
        }
      } catch (_) {
        // a broken connection just ends this handler
      }
      try {
        await socket.flush();
      } catch (_) {}
      try {
        await socket.close();
      } catch (_) {}
    }
    return;
  }
  if (mode == 'client' && transport == 'datagrams') {
    final sock = await RawDatagramSocket.bind(addr, 0); // ephemeral source port
    final cases = await datagramClient(sock, port);
    cases.emit();
    await stdout.flush();
    sock.close();
    return;
  }
  if (mode == 'client') {
    final sock = await connectRetry(port);
    final carrier = SyncStreamFrameCarrier(sock);
    final Cases cases;
    if (transport == 'rpc') {
      cases = rpcClient(carrier);
    } else if (transport == 'events') {
      cases = eventsClient(carrier);
    } else {
      stderr.writeln('bad transport');
      return;
    }
    cases.emit();
    await stdout.flush();
    sock.closeSync();
    return;
  }

  stderr.writeln('bad mode/transport');
  exitCode = 2;
}

/// Print `READY` (flushed) once the server is bound, so the orchestrator's
/// READY-gate releases the client. stdout is block-buffered under a pipe, so the
/// explicit flush matters.
Future<void> announceReady() async {
  stdout.writeln('READY');
  await stdout.flush();
}

/// Bind the TCP listener, retrying briefly so a just-freed port from the previous
/// server in the matrix is tolerated.
Future<ServerSocket> bindRetry(int port) async {
  for (var attempt = 0; attempt < 200; attempt++) {
    try {
      return await ServerSocket.bind(InternetAddress.loopbackIPv4, port);
    } catch (_) {
      await Future<void>.delayed(const Duration(milliseconds: 15));
    }
  }
  return ServerSocket.bind(InternetAddress.loopbackIPv4, port);
}

/// Connect a blocking synchronous TCP client, retrying briefly while the server
/// finishes binding.
Future<RawSynchronousSocket> connectRetry(int port) async {
  for (var attempt = 0; attempt < 400; attempt++) {
    try {
      return RawSynchronousSocket.connectSync(InternetAddress.loopbackIPv4, port);
    } catch (_) {
      await Future<void>.delayed(const Duration(milliseconds: 15));
    }
  }
  return RawSynchronousSocket.connectSync(InternetAddress.loopbackIPv4, port);
}
