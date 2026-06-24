// CSIL-Datagrams transport — unreliable, unordered, message-oriented — see
// `csil-datagrams-transport.md`. CBOR-array (default) and compact fixed-header
// profiles. A datagram channel is single-service: the service is bound at channel
// setup, so datagrams carry no service ordinal.

import 'dart:typed_data';

import 'cbor.dart';
import 'cbor.dart' as cbor show encode, decode;
import 'conventions.dart';

/// Conservative max datagram size (envelope + payload) safe across UDP/WebRTC/QUIC.
const int maxDatagramDefault = 1200;

/// A datagram in the CBOR-array (default) profile: `[v, op_ord, seq, payload]`.
/// All three numeric fields are full CBOR unsigned integers in this profile (the
/// compact-header profile is the one that pins fixed byte widths).
final class Datagram {
  final int opOrd;
  final int seq; // per-channel sequence; 0 means "unsequenced"
  Uint8List payload; // opaque CBOR(message type) bytes

  Datagram(this.opOrd, this.seq, this.payload);

  Uint8List encode() => cbor.encode(CborArray(<Cbor>[
        CborInt(version),
        CborInt(opOrd),
        CborInt(seq),
        tag24(payload),
      ]));

  static Datagram decode(Uint8List bytes) {
    final v = cbor.decode(bytes);
    if (v is! CborArray) {
      throw const MalformedException('datagram is not an array');
    }
    final arr = v.value;
    // Fixed 4-element array — a 3- or 5-element array is rejected, never coerced.
    if (arr.length != 4) {
      throw MalformedException(
          'datagram array has ${arr.length} elements, expected 4');
    }
    checkVersion(_asUint(arr[0]));
    return Datagram(_asUint(arr[1]), _asUint(arr[2]), untag24(arr[3]));
  }

  static int _asUint(Cbor val) {
    if (val is CborInt && val.value >= 0) return val.value;
    throw const MalformedException(
        'datagram field is not a non-negative integer');
  }

  @override
  bool operator ==(Object other) =>
      other is Datagram &&
      other.opOrd == opOrd &&
      other.seq == seq &&
      bytesEqual(other.payload, payload);

  @override
  int get hashCode => Object.hash(opOrd, seq, payload.length);
}

const int _compactVer = 1;
const int _flagEpoch = 0x01;

/// A datagram in the compact fixed-header profile. Header layout:
/// `[ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8])` then the opaque body. The
/// fixed byte widths come from the spec, not the conformance JSON: `op_ord`=u8,
/// `seq`=u16, `epoch`=u8.
final class CompactDatagram {
  final int opOrd; // 0..255
  final int seq; // u16, wraps mod 2^16
  int?
      epoch; // present when the sender tracks restarts (sets the flags epoch bit)
  Uint8List
      body; // opaque body (tag-24 CBOR or a raw media frame, by channel agreement)

  CompactDatagram(this.opOrd, this.seq, this.body);

  CompactDatagram withEpoch(int epoch) {
    this.epoch = epoch;
    return this;
  }

  Uint8List encode() {
    final hasEpoch = epoch != null;
    final flags = hasEpoch ? _flagEpoch : 0;
    final headerLen = hasEpoch ? 5 : 4;
    final out = Uint8List(headerLen + body.length);
    out[0] = (_compactVer << 4) | (flags & 0x0f);
    out[1] = opOrd & 0xff;
    // 16-bit big-endian sequence assembled by hand; the 8-byte `ByteData`
    // methods are the only ones avoided, but doing the u16 by hand keeps the
    // whole header assembly in one obvious place.
    out[2] = (seq >> 8) & 0xff;
    out[3] = seq & 0xff;
    if (hasEpoch) out[4] = epoch! & 0xff;
    out.setRange(headerLen, out.length, body);
    return out;
  }

  static CompactDatagram decode(Uint8List bytes) {
    if (bytes.length < 4) {
      throw const MalformedException(
          'compact datagram shorter than the 4-byte header');
    }
    final ver = bytes[0] >> 4;
    if (ver != _compactVer) throw UnsupportedVersionException(ver);
    final flags = bytes[0] & 0x0f;
    final opOrd = bytes[1];
    final seq = (bytes[2] << 8) | bytes[3];
    int? epoch;
    int bodyStart;
    if ((flags & _flagEpoch) != 0) {
      if (bytes.length < 5) {
        throw const MalformedException(
            'compact datagram flags claim an epoch byte that is absent');
      }
      epoch = bytes[4];
      bodyStart = 5;
    } else {
      bodyStart = 4;
    }
    final d = CompactDatagram(opOrd, seq,
        Uint8List.fromList(Uint8List.sublistView(bytes, bodyStart)));
    if (epoch != null) d.epoch = epoch;
    return d;
  }

  @override
  bool operator ==(Object other) =>
      other is CompactDatagram &&
      other.opOrd == opOrd &&
      other.seq == seq &&
      other.epoch == epoch &&
      bytesEqual(other.body, body);

  @override
  int get hashCode => Object.hash(opOrd, seq, epoch, body.length);
}

/// Classification of an incoming sequence number relative to what was last seen,
/// for loss/reorder/restart detection. The transport detects; the app decides. A
/// `sealed` hierarchy so a host pattern-matches it exhaustively.
sealed class SeqEvent {
  const SeqEvent();
}

final class SeqFirst extends SeqEvent {
  const SeqFirst();
}

final class SeqAdvanced extends SeqEvent {
  final int gap;
  const SeqAdvanced(this.gap);

  @override
  bool operator ==(Object other) => other is SeqAdvanced && other.gap == gap;
  @override
  int get hashCode => gap.hashCode;
}

final class SeqLateOrDuplicate extends SeqEvent {
  const SeqLateOrDuplicate();
}

final class SeqRestart extends SeqEvent {
  const SeqRestart();
}

/// Tracks the last sequence/epoch per channel to classify arrivals. The transport
/// provides the information to detect loss/reorder/restart and nothing more — no
/// recovery, per the spec's design constraints.
final class SeqTracker {
  int? _lastSeq;
  int? _lastEpoch;

  SeqEvent observe(int seq, {int? epoch}) {
    // An epoch change after a *prior* epoch existed means the sender restarted
    // and reset its numbering — distinct from a huge reorder. Going from
    // no-epoch to a first epoch is NOT a restart.
    if (epoch != _lastEpoch && _lastEpoch != null) {
      _lastEpoch = epoch;
      _lastSeq = seq;
      return const SeqRestart();
    }
    _lastEpoch = epoch;
    // seq 0 marks an unsequenced datagram: it carries no ordering information,
    // so it is never late or duplicate. Report a zero-gap advance and leave the
    // running sequence untouched.
    if (seq == 0) {
      return const SeqAdvanced(0);
    }
    if (_lastSeq == null) {
      _lastSeq = seq;
      return const SeqFirst();
    }
    if (seq > _lastSeq!) {
      final gap = seq - _lastSeq! - 1;
      _lastSeq = seq;
      return SeqAdvanced(gap);
    }
    return const SeqLateOrDuplicate();
  }
}
