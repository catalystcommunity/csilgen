// Minimal canonical CBOR codec (RFC 8949). Hand-written and dependency-free so the
// transport library stays offline-testable and Foundation-free (it touches only
// `[UInt8]`, `UInt64`, bit shifts, and `String.utf8` — identical on Linux and Apple).
// It supports exactly what the CSIL envelopes need — unsigned ints, negative ints,
// text strings, byte strings, arrays, maps, and tag 24 — and nothing else. Maps use
// core deterministic encoding: entries sorted by the bytewise lexicographic order of
// their encoded keys, matching the Rust reference's canon_map so bytes are
// byte-identical to the conformance vectors.

/// One key/value pair of a CBOR map, modeled explicitly (rather than as a
/// `Dictionary`) so decode preserves exact entries and round-trips deterministically.
struct CBOREntry: Equatable {
    let key: CBORValue
    let value: CBORValue
}

/// The in-memory model of the CBOR items the envelopes use. Decoding produces these
/// and encoding consumes them; transports build envelopes from the canonical helpers
/// so byte layout is independent of any Swift type's stored-property order.
indirect enum CBORValue: Equatable {
    case uint(UInt64)  // major type 0
    case int(Int64)  // signed; encodes negative values as major type 1
    case bytes([UInt8])  // major type 2
    case text(String)  // major type 3
    case array([CBORValue])  // major type 4
    case map([CBOREntry])  // major type 5
    case tag(UInt64, CBORValue)  // major type 6
}

/// Emit the initial byte (major type in the high three bits) plus the shortest-form
/// argument bytes for `arg`, per deterministic encoding. Big-endian via shifts.
func encodeHead(_ out: inout [UInt8], _ major: UInt8, _ arg: UInt64) {
    let mt = major << 5
    if arg < 24 {
        out.append(mt | UInt8(arg))
    } else if arg < 0x100 {
        out.append(mt | 24)
        out.append(UInt8(arg))
    } else if arg < 0x1_0000 {
        out.append(mt | 25)
        out.append(UInt8((arg >> 8) & 0xff))
        out.append(UInt8(arg & 0xff))
    } else if arg < 0x1_0000_0000 {
        out.append(mt | 26)
        out.append(UInt8((arg >> 24) & 0xff))
        out.append(UInt8((arg >> 16) & 0xff))
        out.append(UInt8((arg >> 8) & 0xff))
        out.append(UInt8(arg & 0xff))
    } else {
        out.append(mt | 27)
        var shift = 56
        while shift >= 0 {
            out.append(UInt8((arg >> UInt64(shift)) & 0xff))
            shift -= 8
        }
    }
}

func encodeInto(_ out: inout [UInt8], _ value: CBORValue) {
    switch value {
    case .uint(let n):
        encodeHead(&out, 0, n)
    case .int(let i):
        if i >= 0 {
            encodeHead(&out, 0, UInt64(i))
        } else {
            // CBOR negative ints encode -1-n; ~i is exactly -1-i in two's complement,
            // and its bit pattern as UInt64 is the magnitude without an overflow trap
            // (even for Int64.min, where -i would trap).
            encodeHead(&out, 1, UInt64(bitPattern: ~i))
        }
    case .bytes(let b):
        encodeHead(&out, 2, UInt64(b.count))
        out.append(contentsOf: b)
    case .text(let s):
        let b = Array(s.utf8)
        encodeHead(&out, 3, UInt64(b.count))
        out.append(contentsOf: b)
    case .array(let items):
        encodeHead(&out, 4, UInt64(items.count))
        for item in items {
            encodeInto(&out, item)
        }
    case .map(let entries):
        encodeHead(&out, 5, UInt64(entries.count))
        for entry in entries {
            encodeInto(&out, entry.key)
            encodeInto(&out, entry.value)
        }
    case .tag(let num, let content):
        encodeHead(&out, 6, num)
        encodeInto(&out, content)
    }
}

/// Serialize a value to canonical CBOR bytes.
func encodeValue(_ value: CBORValue) -> [UInt8] {
    var out: [UInt8] = []
    encodeInto(&out, value)
    return out
}

/// Bytewise lexicographic ordering of two encoded keys (shorter-first falls out of
/// the length-prefixed head byte, so this reproduces the conventions doc ordering).
func lexLess(_ a: [UInt8], _ b: [UInt8]) -> Bool {
    let n = min(a.count, b.count)
    var i = 0
    while i < n {
        if a[i] != b[i] {
            return a[i] < b[i]
        }
        i += 1
    }
    return a.count < b.count
}

/// Build a deterministically-keyed CBOR map: entries sorted by the bytewise
/// lexicographic order of their *encoded* keys (RFC 8949 §4.2.1), so the same logical
/// envelope always yields identical bytes.
func canonMap(_ entries: [CBOREntry]) -> CBORValue {
    let sorted = entries.sorted { lhs, rhs in
        lexLess(encodeValue(lhs.key), encodeValue(rhs.key))
    }
    return .map(sorted)
}

/// Decode a complete envelope: one self-contained CBOR item with no trailing bytes.
/// An envelope is a single CBOR item, so any leftover bytes are a malformed frame and
/// rejected — matching the Rust and Go references rather than silently ignoring them.
func decodeEnvelope(_ b: [UInt8]) throws -> CBORValue {
    let (value, next) = try decodeValue(b, 0, 0)
    guard next == b.count else {
        throw TransportError.decode("trailing bytes after CBOR item")
    }
    return value
}

/// Parse one CBOR item starting at `start`, returning it and the index just past it.
/// The recursive workhorse; envelope decoders use `decodeEnvelope`, which additionally
/// rejects trailing bytes.
func decodeValue(_ b: [UInt8], _ start: Int, _ depth: Int) throws -> (CBORValue, Int) {
    guard depth <= 64 else {
        throw TransportError.decode("CBOR nesting limit exceeded")
    }
    guard start < b.count else {
        throw TransportError.decode("unexpected end of input")
    }
    let initial = b[start]
    let major = initial >> 5
    let low = initial & 0x1f
    let (arg, headLen) = try readArg(b, start, low)
    let off = start + headLen
    switch major {
    case 0:
        return (.uint(arg), off)
    case 1:
        // An arg above Int64.max names a value below Int64.min, which would wrap if
        // forced into an Int64; reject it instead (mirrors the Go reference guard).
        guard arg <= UInt64(Int64.max) else {
            throw TransportError.decode("negative integer out of Int64 range")
        }
        return (.int(-1 - Int64(arg)), off)
    case 2:
        let end = try sliceEnd(b, off, arg, "byte string")
        return (.bytes(Array(b[off..<end])), end)
    case 3:
        let end = try sliceEnd(b, off, arg, "text string")
        guard let text = String(validating: b[off..<end], as: UTF8.self) else {
            throw TransportError.decode("invalid UTF-8 text string")
        }
        return (.text(text), end)
    case 4:
        guard arg <= UInt64(b.count - off) else {
            throw TransportError.decode("array length exceeds remaining input")
        }
        var items: [CBORValue] = []
        items.reserveCapacity(Int(min(arg, 1024)))
        var cursor = off
        var i: UInt64 = 0
        while i < arg {
            let (item, next) = try decodeValue(b, cursor, depth + 1)
            items.append(item)
            cursor = next
            i += 1
        }
        return (.array(items), cursor)
    case 5:
        guard arg <= UInt64(b.count - off) else {
            throw TransportError.decode("map length exceeds remaining input")
        }
        var entries: [CBOREntry] = []
        entries.reserveCapacity(Int(min(arg, 1024)))
        var cursor = off
        var i: UInt64 = 0
        while i < arg {
            let (key, afterKey) = try decodeValue(b, cursor, depth + 1)
            let (value, afterValue) = try decodeValue(b, afterKey, depth + 1)
            entries.append(CBOREntry(key: key, value: value))
            cursor = afterValue
            i += 1
        }
        return (.map(entries), cursor)
    case 6:
        let (content, next) = try decodeValue(b, off, depth + 1)
        return (.tag(arg, content), next)
    default:
        throw TransportError.decode("unsupported major type \(major)")
    }
}

/// Validate and compute the end index of a length-delimited string item.
private func sliceEnd(_ b: [UInt8], _ off: Int, _ length: UInt64, _ what: String) throws -> Int {
    guard length <= UInt64(b.count - off) else {
        throw TransportError.decode("truncated \(what)")
    }
    return off + Int(length)
}

/// Read the additional-information argument for a head byte whose low five bits are
/// `low`, returning the argument value and the total head length (including the
/// initial byte).
func readArg(_ b: [UInt8], _ start: Int, _ low: UInt8) throws -> (UInt64, Int) {
    switch low {
    case 0...23:
        return (UInt64(low), 1)
    case 24:
        guard start >= 0, start < b.count, b.count - start > 1 else {
            throw TransportError.decode("truncated 1-byte argument")
        }
        return (UInt64(b[start + 1]), 2)
    case 25:
        guard start >= 0, start < b.count, b.count - start > 2 else {
            throw TransportError.decode("truncated 2-byte argument")
        }
        return (UInt64(b[start + 1]) << 8 | UInt64(b[start + 2]), 3)
    case 26:
        guard start >= 0, start < b.count, b.count - start > 4 else {
            throw TransportError.decode("truncated 4-byte argument")
        }
        let v =
            UInt64(b[start + 1]) << 24 | UInt64(b[start + 2]) << 16 | UInt64(b[start + 3]) << 8
            | UInt64(b[start + 4])
        return (v, 5)
    case 27:
        guard start >= 0, start < b.count, b.count - start > 8 else {
            throw TransportError.decode("truncated 8-byte argument")
        }
        var v: UInt64 = 0
        var i = 1
        while i <= 8 {
            v = (v << 8) | UInt64(b[start + i])
            i += 1
        }
        return (v, 9)
    default:
        // 28..31 (indefinite-length / reserved) are forbidden in CSIL envelopes.
        throw TransportError.decode("indefinite or reserved additional info \(low)")
    }
}
