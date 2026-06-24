// CSIL-Events transport — typed bidirectional event streams — see
// `csil-events-transport.md`. Verbose (text-keyed) and compact (positional)
// profiles, plus the control plane (service ordinal 0) lifecycle payloads.

import 'dart:typed_data';

import 'cbor.dart';
import 'cbor.dart' as cbor show encode, decode;
import 'conventions.dart';

/// Which wire profile a connection uses for its lifetime, fixed by `hello`. A
/// genuinely closed two-member set, so a Dart `enum` (unlike the transport
/// `Status` registry) is the right model.
enum Profile { verbose, compact }

Profile? parseProfile(String s) => switch (s) {
      'verbose' => Profile.verbose,
      'compact' => Profile.compact,
      _ => null,
    };

/// Control-plane operation ordinals (under service ordinal 0) and their `$`-sigil
/// verbose names.
abstract final class Control {
  static const int hello = 0;
  static const int helloAck = 1;
  static const int ping = 2;
  static const int pong = 3;
  static const int close = 4;
  static const int error = 5;

  static const String helloName = r'$hello';
  static const String helloAckName = r'$hello-ack';
  static const String pingName = r'$ping';
  static const String pongName = r'$pong';
  static const String closeName = r'$close';
  static const String errorName = r'$error';
}

/// One typed event flowing in either direction. Identified by service+operation
/// (verbose) or by their ordinals (compact); carries an optional correlation `id`
/// when it is a request expecting a reply, or that reply.
final class Event {
  String?
      service; // CSIL service name (verbose); omitted on a single-service connection
  int?
      serviceOrd; // service ordinal (compact); always present in compact frames
  String? event; // CSIL operation name (verbose)
  int? opOrd; // operation ordinal (compact)
  int? id;
  Uint8List payload;

  Event._(this.payload);

  /// A verbose event by name. `service` is omitted on a single-service connection.
  factory Event.verbose(String? service, String event, Uint8List payload) {
    final e = Event._(payload);
    e.service = service;
    e.event = event;
    return e;
  }

  /// A compact event by ordinals.
  factory Event.compact(int serviceOrd, int opOrd, Uint8List payload) {
    final e = Event._(payload);
    e.serviceOrd = serviceOrd;
    e.opOrd = opOrd;
    return e;
  }

  Event withId(int id) {
    this.id = id;
    return this;
  }

  Uint8List encode(Profile profile) =>
      profile == Profile.verbose ? _encodeVerbose() : _encodeCompact();

  Uint8List _encodeVerbose() {
    final name = event;
    if (name == null) {
      throw const MalformedException("verbose event missing 'event' name");
    }
    final entries = <(String, Cbor)>[
      ('event', CborText(name)),
      ('payload', tag24(payload)),
    ];
    if (service != null) entries.add(('service', CborText(service!)));
    if (id != null) entries.add(('id', CborInt(id!)));
    return cbor.encode(canonMap(entries));
  }

  Uint8List _encodeCompact() {
    final sOrd = serviceOrd;
    final oOrd = opOrd;
    if (sOrd == null) {
      throw const MalformedException('compact event missing service ordinal');
    }
    if (oOrd == null) {
      throw const MalformedException('compact event missing op ordinal');
    }
    final arr = <Cbor>[CborInt(sOrd), CborInt(oOrd)];
    if (id != null) arr.add(CborInt(id!));
    arr.add(tag24(payload));
    return cbor.encode(CborArray(arr));
  }

  static Event decode(Uint8List bytes, Profile profile) =>
      profile == Profile.verbose
          ? _decodeVerbose(bytes)
          : _decodeCompact(bytes);

  static Event _decodeVerbose(Uint8List bytes) {
    final v = cbor.decode(bytes);
    final payloadField = mapGet(v, 'payload');
    if (payloadField == null) {
      throw const MalformedException("missing 'payload'");
    }
    final e = Event.verbose(
        getTextOpt(v, 'service'), getText(v, 'event'), untag24(payloadField));
    e.id = getUintOpt(v, 'id');
    return e;
  }

  static Event _decodeCompact(Uint8List bytes) {
    final v = cbor.decode(bytes);
    if (v is! CborArray) {
      throw const MalformedException('compact event is not an array');
    }
    final arr = v.value;
    // 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
    Cbor payloadVal;
    Cbor? idVal;
    if (arr.length == 3) {
      payloadVal = arr[2];
    } else if (arr.length == 4) {
      idVal = arr[2];
      payloadVal = arr[3];
    } else {
      throw MalformedException(
          'compact event array has ${arr.length} elements, expected 3 or 4');
    }
    final e =
        Event.compact(_asUint(arr[0]), _asUint(arr[1]), untag24(payloadVal));
    if (idVal != null) e.id = _asUint(idVal);
    return e;
  }

  static int _asUint(Cbor val) {
    if (val is CborInt && val.value >= 0) return val.value;
    throw const MalformedException('ordinal is not a non-negative integer');
  }

  @override
  bool operator ==(Object other) =>
      other is Event &&
      other.service == service &&
      other.serviceOrd == serviceOrd &&
      other.event == event &&
      other.opOrd == opOrd &&
      other.id == id &&
      bytesEqual(other.payload, payload);

  @override
  int get hashCode =>
      Object.hash(service, serviceOrd, event, opOrd, id, payload.length);
}

/// The `$hello` payload offered by the connection initiator.
final class Hello {
  final List<int> versions;
  final List<String> profiles;
  String? service;
  String? auth;

  Hello(this.versions, this.profiles, {this.service, this.auth});

  Uint8List encode() {
    final entries = <(String, Cbor)>[
      ('versions', CborArray(versions.map<Cbor>((n) => CborInt(n)).toList())),
      ('profiles', CborArray(profiles.map<Cbor>((p) => CborText(p)).toList())),
    ];
    if (service != null) entries.add(('service', CborText(service!)));
    if (auth != null) entries.add(('auth', CborText(auth!)));
    return cbor.encode(canonMap(entries));
  }

  static Hello decode(Uint8List bytes) {
    final v = cbor.decode(bytes);
    final versionsField = mapGet(v, 'versions');
    if (versionsField is! CborArray) {
      throw const MalformedException("hello missing 'versions'");
    }
    final versions = versionsField.value
        .whereType<CborInt>()
        .map((x) => x.value)
        .toList(growable: false);
    final profilesField = mapGet(v, 'profiles');
    if (profilesField is! CborArray) {
      throw const MalformedException("hello missing 'profiles'");
    }
    final profiles = profilesField.value
        .whereType<CborText>()
        .map((x) => x.value)
        .toList(growable: false);
    return Hello(versions, profiles,
        service: getTextOpt(v, 'service'), auth: getTextOpt(v, 'auth'));
  }

  /// Select a profile from this hello's offers, honoring the peer's preference
  /// order and what the server supports. Returns `(version, profile)`, or `null`
  /// if nothing is mutually supported.
  (int, Profile)? negotiate(List<Profile> supported) {
    if (!versions.contains(version)) return null;
    for (final offered in profiles) {
      final p = parseProfile(offered);
      if (p != null && supported.contains(p)) return (version, p);
    }
    return null;
  }
}

/// The `$hello-ack` payload returned by the peer.
final class HelloAck {
  final int v;
  final String profile;
  String? session;

  HelloAck(this.v, this.profile, {this.session});

  Uint8List encode() {
    final entries = <(String, Cbor)>[
      ('v', CborInt(v)),
      ('profile', CborText(profile)),
    ];
    if (session != null) entries.add(('session', CborText(session!)));
    return cbor.encode(canonMap(entries));
  }

  static HelloAck decode(Uint8List bytes) {
    final value = cbor.decode(bytes);
    return HelloAck(getUint(value, 'v'), getText(value, 'profile'),
        session: getTextOpt(value, 'session'));
  }
}

/// A `$ping`/`$pong` heartbeat payload.
final class Heartbeat {
  final int nonce;
  int? at;

  Heartbeat(this.nonce, {this.at});

  Uint8List encode() {
    final entries = <(String, Cbor)>[('nonce', CborInt(nonce))];
    if (at != null) entries.add(('at', CborInt(at!)));
    return cbor.encode(canonMap(entries));
  }

  static Heartbeat decode(Uint8List bytes) {
    final v = cbor.decode(bytes);
    return Heartbeat(getUint(v, 'nonce'), at: getUintOpt(v, 'at'));
  }
}

/// A `$close` payload. `status` is a transport-registry code.
final class Close {
  final Status status;
  String? reason;

  Close(this.status, {this.reason});

  Uint8List encode() {
    final entries = <(String, Cbor)>[('status', CborInt(status.code))];
    if (reason != null) entries.add(('reason', CborText(reason!)));
    return cbor.encode(canonMap(entries));
  }

  static Close decode(Uint8List bytes) {
    final v = cbor.decode(bytes);
    return Close(Status(getInt(v, 'status')), reason: getTextOpt(v, 'reason'));
  }
}
