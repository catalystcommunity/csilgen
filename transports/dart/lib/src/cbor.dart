// Minimal canonical CBOR codec (RFC 8949) — hand-rolled, zero dependencies.
//
// Only the subset the CSIL transports use is implemented: unsigned and negative
// integers, text strings, byte strings, arrays, maps, and tags. Maps are emitted
// with RFC 8949 §4.2.1 core deterministic encoding — entries sorted by the
// bytewise lexicographic order of their *encoded* keys — so the bytes match the
// conformance vectors and the Rust/Go/TS references.
//
// Everything works on `Uint8List`; there is no Node/Flutter surface here so the
// codec runs identically on the Dart VM and on Flutter Web (dart2js).

import 'dart:convert';
import 'dart:typed_data';

import 'conventions.dart' show DecodeException, EncodeException;

// The largest integer a Dart `int` represents exactly on the web (it is a JS
// double there: 53 bits). Decoding a CBOR integer above this would silently lose
// precision on the web, so the codec rejects it instead — no envelope field is
// ever this large, matching the TS reference's choice.
const int maxSafeInteger = 0x1fffffffffffff; // 2^53 - 1

/// The CBOR value model. A single `CborInt` covers both major type 0 (unsigned)
/// and 1 (negative); the encoder picks the major type by sign, matching how the
/// Rust reference's `Value::Integer` collapses the two.
sealed class Cbor {
  const Cbor();
}

final class CborInt extends Cbor {
  final int value;
  const CborInt(this.value);
}

final class CborBytes extends Cbor {
  final Uint8List value;
  const CborBytes(this.value);
}

final class CborText extends Cbor {
  final String value;
  const CborText(this.value);
}

final class CborArray extends Cbor {
  final List<Cbor> value;
  const CborArray(this.value);
}

final class CborMap extends Cbor {
  final List<(Cbor, Cbor)> entries;
  const CborMap(this.entries);
}

final class CborTag extends Cbor {
  final int tag;
  final Cbor value;
  const CborTag(this.tag, this.value);
}

// A growable byte sink built into a `BytesBuilder`.
class _Writer {
  final BytesBuilder _out = BytesBuilder(copy: false);

  void byte(int b) => _out.addByte(b & 0xff);
  void raw(Uint8List src) => _out.add(src);

  // Emit a CBOR head: the major type in the top three bits, then the shortest
  // additional-information encoding of `arg` that fits — the determinism rule.
  void head(int major, int arg) {
    final mt = major << 5;
    if (arg < 24) {
      byte(mt | arg);
    } else if (arg < 0x100) {
      byte(mt | 24);
      byte(arg);
    } else if (arg < 0x10000) {
      byte(mt | 25);
      byte(arg >> 8);
      byte(arg);
    } else if (arg < 0x100000000) {
      byte(mt | 26);
      byte(arg >> 24);
      byte(arg >> 16);
      byte(arg >> 8);
      byte(arg);
    } else {
      // Beyond 2^32 the 8-byte head is assembled via `BigInt` rather than the
      // 64-bit `ByteData` methods, which throw `UnsupportedError` on dart2js.
      byte(mt | 27);
      final big = BigInt.from(arg);
      final mask = BigInt.from(0xff);
      for (var shift = 56; shift >= 0; shift -= 8) {
        byte(((big >> shift) & mask).toInt());
      }
    }
  }

  Uint8List finish() => _out.toBytes();
}

void _writeValue(_Writer w, Cbor value) {
  switch (value) {
    case CborInt(value: final v):
      if (v < -maxSafeInteger - 1 || v > maxSafeInteger) {
        throw EncodeException('integer $v is outside the safe integer range');
      }
      if (v >= 0) {
        w.head(0, v);
      } else {
        w.head(1, -1 - v);
      }
    case CborBytes(value: final v):
      w.head(2, v.length);
      w.raw(v);
    case CborText(value: final v):
      final utf8Bytes = Uint8List.fromList(utf8.encode(v));
      w.head(3, utf8Bytes.length);
      w.raw(utf8Bytes);
    case CborArray(value: final items):
      w.head(4, items.length);
      for (final item in items) {
        _writeValue(w, item);
      }
    case CborMap(entries: final entries):
      // Core deterministic encoding: sort entries by the bytewise lexicographic
      // order of their *encoded* keys, then emit. A logically equal map is
      // therefore byte-equal regardless of construction order.
      final encoded = entries
          .map((e) => (key: encode(e.$1), val: encode(e.$2)))
          .toList(growable: false)
        ..sort((a, b) => compareBytes(a.key, b.key));
      w.head(5, entries.length);
      for (final e in encoded) {
        w.raw(e.key);
        w.raw(e.val);
      }
    case CborTag(tag: final tag, value: final inner):
      w.head(6, tag);
      _writeValue(w, inner);
  }
}

/// Encode a single CBOR value to canonical bytes.
Uint8List encode(Cbor value) {
  final w = _Writer();
  _writeValue(w, value);
  return w.finish();
}

/// Bytewise lexicographic comparison; a shorter run that is a prefix of the
/// longer sorts first, matching `Vec<u8>::cmp` on the Rust side.
int compareBytes(Uint8List a, Uint8List b) {
  final n = a.length < b.length ? a.length : b.length;
  for (var i = 0; i < n; i++) {
    if (a[i] != b[i]) return a[i] - b[i];
  }
  return a.length - b.length;
}

class _Reader {
  final Uint8List data;
  int pos = 0;
  _Reader(this.data);

  bool get atEnd => pos == data.length;
  int get remaining => data.length - pos;

  int _byte() {
    if (pos >= data.length) {
      throw DecodeException('unexpected end of CBOR input');
    }
    return data[pos++];
  }

  Uint8List _take(int n) {
    if (pos + n > data.length) {
      throw DecodeException('unexpected end of CBOR input');
    }
    final slice = Uint8List.sublistView(data, pos, pos + n);
    pos += n;
    return slice;
  }

  // Read the additional-information argument for a head byte. Indefinite-length
  // (ai 31) and the reserved 28–30 are rejected — the conventions forbid them.
  int _argument(int ai) {
    if (ai < 24) return ai;
    if (ai == 24) return _byte();
    if (ai == 25) return (_byte() << 8) | _byte();
    if (ai == 26) {
      // Assemble unsigned so the high bit cannot turn the result negative.
      return _byte() * 0x1000000 + (_byte() << 16) + (_byte() << 8) + _byte();
    }
    if (ai == 27) {
      // The 8-byte argument is assembled via `BigInt` (never `getUint64`, which
      // throws on dart2js). A value above 2^53 cannot survive as a web `int`, so
      // it is rejected rather than silently truncated — no envelope needs it.
      var big = BigInt.zero;
      for (var i = 0; i < 8; i++) {
        big = (big << 8) | BigInt.from(_byte());
      }
      if (big > BigInt.from(maxSafeInteger)) {
        throw DecodeException('CBOR integer exceeds safe integer range');
      }
      return big.toInt();
    }
    throw DecodeException(
        'unsupported CBOR additional info $ai (indefinite lengths are not allowed)');
  }

  Cbor value([int depth = 0]) {
    if (depth > 64) {
      throw DecodeException('CBOR nesting limit exceeded');
    }
    final head = _byte();
    final major = head >> 5;
    final arg = _argument(head & 0x1f);
    switch (major) {
      case 0:
        return CborInt(arg);
      case 1:
        return CborInt(-1 - arg);
      case 2:
        return CborBytes(Uint8List.fromList(_take(arg)));
      case 3:
        return CborText(utf8.decode(_take(arg)));
      case 4:
        if (arg > remaining) {
          throw DecodeException('CBOR array length exceeds remaining input');
        }
        final items = <Cbor>[];
        for (var i = 0; i < arg; i++) {
          items.add(value(depth + 1));
        }
        return CborArray(items);
      case 5:
        if (arg > remaining) {
          throw DecodeException('CBOR map length exceeds remaining input');
        }
        final entries = <(Cbor, Cbor)>[];
        for (var i = 0; i < arg; i++) {
          final k = value(depth + 1);
          final v = value(depth + 1);
          entries.add((k, v));
        }
        return CborMap(entries);
      case 6:
        return CborTag(arg, value(depth + 1));
      default:
        throw DecodeException('unsupported CBOR major type $major');
    }
  }
}

/// Decode a single CBOR data item from `data`. An envelope is one self-contained
/// CBOR item (conventions doc §1), so any bytes left after the item are rejected
/// rather than silently ignored — matching the Rust/Go/TS decoders.
Cbor decode(Uint8List data) {
  final reader = _Reader(data);
  final value = reader.value();
  if (!reader.atEnd) {
    throw DecodeException('trailing bytes after CBOR item');
  }
  return value;
}
