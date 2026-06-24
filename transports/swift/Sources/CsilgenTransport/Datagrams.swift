// CSIL-Datagrams transport — unreliable, unordered, message-oriented — see
// csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header profiles.
// A datagram channel is single-service: the service is bound at channel setup, so
// datagrams carry no service ordinal.

/// A datagram in the CBOR-array (default) profile: `[v, op_ord, seq, payload]`. Every
/// field is a full CBOR uint here (unlike the compact profile's fixed widths), so the
/// op_ord/seq carry no width ceiling and the payload is the usual tag-24 byte string.
public struct Datagram: Equatable, Sendable {
    public var opOrd: UInt64
    /// The per-channel sequence; 0 means "unsequenced".
    public var seq: UInt64
    /// The opaque CBOR(message type) bytes (wrapped in tag 24 on the wire).
    public var payload: [UInt8]

    public init(opOrd: UInt64, seq: UInt64, payload: [UInt8]) {
        self.opOrd = opOrd
        self.seq = seq
        self.payload = payload
    }

    public func encode() -> [UInt8] {
        encodeValue(.array([.uint(csilVersion), .uint(opOrd), .uint(seq), tag24(payload)]))
    }

    public static func decode(_ b: [UInt8]) throws -> Datagram {
        let v = try decodeEnvelope(b)
        guard case .array(let arr) = v else { throw malformed("datagram is not an array") }
        guard arr.count == 4 else {
            throw malformed("datagram array has \(arr.count) elements, expected 4")
        }
        guard let ver = asU64(arr[0]), let opOrd = asU64(arr[1]), let seq = asU64(arr[2]) else {
            throw malformed("datagram field not an integer")
        }
        try checkVersion(ver)
        return Datagram(opOrd: opOrd, seq: seq, payload: try untag24(arr[3]))
    }
}

/// A datagram in the compact fixed-header profile. The header is a hand-packed binary
/// layout (NOT CBOR) so the hot media path never parses CBOR for the header:
/// `[ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8])` then the opaque body. The fixed
/// widths come from csil-datagrams-transport.md §2.2, not from the conformance JSON
/// (which leaves its numbers untyped).
public struct CompactDatagram: Equatable, Sendable {
    public var opOrd: UInt8
    public var seq: UInt16
    /// Present when the sender tracks restarts (sets the flags epoch bit).
    public var epoch: UInt8?
    /// The opaque body bytes (tag-24 CBOR or a raw media frame, by channel agreement).
    public var body: [UInt8]

    public init(opOrd: UInt8, seq: UInt16, body: [UInt8], epoch: UInt8? = nil) {
        self.opOrd = opOrd
        self.seq = seq
        self.body = body
        self.epoch = epoch
    }

    /// Set the epoch byte (and the flags bit that signals its presence).
    public func withEpoch(_ epoch: UInt8) -> CompactDatagram {
        var copy = self
        copy.epoch = epoch
        return copy
    }

    private static let compactVer: UInt8 = 1
    private static let flagEpoch: UInt8 = 0b0001

    public func encode() -> [UInt8] {
        let flags: UInt8 = epoch != nil ? Self.flagEpoch : 0
        var out: [UInt8] = []
        out.reserveCapacity(5 + body.count)
        out.append((Self.compactVer << 4) | (flags & 0x0f))
        out.append(opOrd)
        out.append(UInt8(seq >> 8))
        out.append(UInt8(seq & 0xff))
        if let epoch { out.append(epoch) }
        out.append(contentsOf: body)
        return out
    }

    public static func decode(_ b: [UInt8]) throws -> CompactDatagram {
        guard b.count >= 4 else {
            throw malformed("compact datagram shorter than the 4-byte header")
        }
        let ver = b[0] >> 4
        guard ver == compactVer else { throw TransportError.unsupportedVersion(UInt64(ver)) }
        let flags = b[0] & 0x0f
        let opOrd = b[1]
        let seq = UInt16(b[2]) << 8 | UInt16(b[3])
        var epoch: UInt8? = nil
        var bodyStart = 4
        if flags & flagEpoch != 0 {
            guard b.count >= 5 else {
                throw malformed("compact datagram flags claim an epoch byte that is absent")
            }
            epoch = b[4]
            bodyStart = 5
        }
        return CompactDatagram(
            opOrd: opOrd, seq: seq, body: Array(b[bodyStart...]), epoch: epoch)
    }
}

/// Classifies an incoming sequence number relative to what was last seen, for
/// loss/reorder/restart detection. The transport detects; the app decides policy.
public enum SeqEventKind: Equatable, Sendable {
    /// The first datagram seen on the channel.
    case first
    /// Strictly newer than the last (possibly skipping some — a gap/loss), with the
    /// count of skipped sequence numbers.
    case advanced(gap: UInt64)
    /// Not newer than the last seen (a late or duplicate datagram).
    case lateOrDuplicate
    /// The sender restarted (epoch changed); seq numbering reset.
    case restart
}

/// Tracks the last sequence/epoch per channel to classify arrivals. Unsequenced
/// datagrams (seq 0) are reported as `.advanced(gap: 0)`.
public final class SeqTracker {
    private var lastSeq: UInt64?
    private var lastEpoch: UInt8?

    public init() {}

    /// Classify an arriving (seq, epoch) and update the tracker state.
    public func observe(seq: UInt64, epoch: UInt8?) -> SeqEventKind {
        // A restart fires only when a *prior* epoch existed and changed; going from
        // no-epoch to a first epoch is not a restart.
        if epoch != lastEpoch, lastEpoch != nil {
            lastEpoch = epoch
            lastSeq = seq
            return .restart
        }
        lastEpoch = epoch
        // seq 0 marks an unsequenced datagram: it carries no ordering information, so it
        // is never late or duplicate. Report a zero-gap advance and leave the running
        // sequence untouched so a mix of sequenced and unsequenced still tracks the
        // sequenced ones.
        if seq == 0 {
            return .advanced(gap: 0)
        }
        guard let last = lastSeq else {
            lastSeq = seq
            return .first
        }
        if seq > last {
            lastSeq = seq
            return .advanced(gap: seq - last - 1)
        }
        return .lateOrDuplicate
    }
}
