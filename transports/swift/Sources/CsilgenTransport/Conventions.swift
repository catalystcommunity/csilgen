// Conventions shared by every CSIL transport — see csil-transport-conventions.md.
//
// This file owns the parts the three transports agree on: the version constant, the
// transport status registry, tag-24 payload wrap/unwrap, the max-frame guard, and the
// canonical-CBOR field accessors the envelopes build on so their bytes match the
// conformance vectors regardless of Swift stored-property order.

/// The current transport version. A new value is minted only for a breaking change to
/// envelope layout or semantics.
public let csilVersion: UInt64 = 1

/// The CBOR semantic tag wrapping an embedded, opaque CBOR data item (RFC 8949
/// §3.4.5.1).
public let tagEncodedCbor: UInt64 = 24

/// The reserved service ordinal for the transport control plane (Events lifecycle).
public let controlServiceOrd: UInt64 = 0

/// The default max encoded envelope size for stream/message carriers (16 MiB). A
/// carrier rejects anything larger before allocating for it.
public let maxFrameDefault: Int = 16 * 1024 * 1024

/// The conservative max datagram size (envelope + payload) safe across UDP/WebRTC/QUIC.
public let maxDatagramDefault: Int = 1200

/// Errors raised across the transport layer. Malformed wire input is *expected* and
/// must throw — never `fatalError`/`assert`, which are for programmer errors only.
public enum TransportError: Error, Equatable {
    case encode(String)
    case decode(String)
    case malformed(String)
    case frameTooLarge(got: Int, maximum: Int)
    case unsupportedVersion(UInt64)
    /// A non-zero transport status returned by a peer (distinct from an application
    /// error, which rides inside the payload as a declared `/ ErrorType` arm).
    case status(name: String, code: Int, message: String)
    case carrier(String)
}

/// A transport-level status. It is distinct from application errors. Modeled as a
/// `struct` over an `Int` (NOT a closed enum) so host-defined extension codes (>= 64)
/// and otherwise-unknown codes round-trip verbatim. Equality is by the code.
public struct Status: Equatable, Sendable {
    public let code: Int

    public init(code: Int) {
        self.code = code
    }

    // The registry codes (conventions doc §4).
    public static let ok = Status(code: 0)
    public static let malformedEnvelope = Status(code: 1)
    public static let unknownServiceOrOp = Status(code: 2)
    public static let unauthenticated = Status(code: 3)
    public static let forbidden = Status(code: 4)
    public static let versionUnsupported = Status(code: 5)
    public static let `internal` = Status(code: 6)
    public static let unavailable = Status(code: 7)
    public static let deadlineExceeded = Status(code: 8)

    /// Whether the status indicates a typed reply is present.
    public var isOk: Bool { code == 0 }

    /// The registry name for the status, or "other" for codes outside it.
    public var name: String {
        switch code {
        case 0: return "ok"
        case 1: return "malformed-envelope"
        case 2: return "unknown-service-or-op"
        case 3: return "unauthenticated"
        case 4: return "forbidden"
        case 5: return "version-unsupported"
        case 6: return "internal"
        case 7: return "unavailable"
        case 8: return "deadline-exceeded"
        default: return "other"
        }
    }
}

/// A malformed-envelope error with a formatted reason.
func malformed(_ reason: String) -> TransportError {
    TransportError.malformed(reason)
}

/// Wrap opaque payload bytes (themselves a CBOR item) in tag 24.
func tag24(_ payload: [UInt8]) -> CBORValue {
    .tag(tagEncodedCbor, .bytes(payload))
}

/// Extract the opaque payload bytes from a tag-24 value.
func untag24(_ value: CBORValue) throws -> [UInt8] {
    guard case .tag(let num, let content) = value, num == tagEncodedCbor else {
        throw malformed("expected a tag-24 (encoded-cbor) payload")
    }
    guard case .bytes(let b) = content else {
        throw malformed("tag-24 payload is not a byte string")
    }
    return b
}

/// Look up a text key in a CBOR map value.
func mapGet(_ value: CBORValue, _ key: String) -> CBORValue? {
    guard case .map(let entries) = value else {
        return nil
    }
    for entry in entries {
        if case .text(let t) = entry.key, t == key {
            return entry.value
        }
    }
    return nil
}

/// Read a non-negative integer from a decoded CBOR integer value.
func asU64(_ value: CBORValue) -> UInt64? {
    switch value {
    case .uint(let n): return n
    case .int(let n) where n >= 0: return UInt64(n)
    default: return nil
    }
}

/// Read a signed integer from a decoded CBOR integer value.
func asI64(_ value: CBORValue) -> Int64? {
    switch value {
    case .uint(let n) where n <= UInt64(Int64.max): return Int64(n)
    case .int(let n): return n
    default: return nil
    }
}

func getUint(_ map: CBORValue, _ key: String) throws -> UInt64 {
    guard let v = mapGet(map, key), let n = asU64(v) else {
        throw malformed("missing or non-integer field '\(key)'")
    }
    return n
}

func getInt(_ map: CBORValue, _ key: String) throws -> Int64 {
    guard let v = mapGet(map, key), let n = asI64(v) else {
        throw malformed("missing or non-integer field '\(key)'")
    }
    return n
}

func getText(_ map: CBORValue, _ key: String) throws -> String {
    guard let v = mapGet(map, key), case .text(let t) = v else {
        throw malformed("missing or non-text field '\(key)'")
    }
    return t
}

func getTextOpt(_ map: CBORValue, _ key: String) -> String? {
    guard let v = mapGet(map, key), case .text(let t) = v else {
        return nil
    }
    return t
}

func getUintOpt(_ map: CBORValue, _ key: String) -> UInt64? {
    guard let v = mapGet(map, key), let n = asU64(v) else {
        return nil
    }
    return n
}

/// Verify a decoded envelope's version field so an unknown version is never silently
/// misparsed.
func checkVersion(_ v: UInt64) throws {
    guard v == csilVersion else {
        throw TransportError.unsupportedVersion(v)
    }
}
