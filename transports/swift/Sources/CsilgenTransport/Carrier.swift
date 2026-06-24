// Carrier seams — the bring-your-own-carrier boundary (conventions doc §7).
//
// The library owns envelope codecs, framing, and lifecycle; the *carrier* (the
// byte/datagram transport) is injected. A host supplies QUIC, WebRTC, a platform media
// stack, or anything else by conforming to one of these protocols — without changing
// the library. Every method is SYNCHRONOUS and throwing; the host owns the I/O loop
// and any thread it runs on. There is no async, ever.

/// Sends and receives one *delimited message* at a time. Used by CSIL-RPC and
/// CSIL-Events. Built-in implementations frame with a 4-byte big-endian length prefix;
/// a host may implement this over WebSocket binary frames, a WebTransport stream, etc.
public protocol FrameCarrier {
    func sendFrame(_ frame: [UInt8]) throws
    /// The next frame, or nil at a clean end of stream.
    func recvFrame() throws -> [UInt8]?
}

/// Sends and receives one self-contained datagram (each within the channel MTU), with
/// no delivery or ordering guarantee. Used by CSIL-Datagrams. A host plugs WebRTC
/// unreliable channels, QUIC datagrams, UDP, etc.
public protocol DatagramCarrier {
    func sendDatagram(_ datagram: [UInt8]) throws
    /// The next datagram, or nil if the carrier is closed.
    func recvDatagram() throws -> [UInt8]?
}

/// A byte stream the built-in `StreamCarrier` frames over (TCP, TLS, a Unix socket, an
/// in-memory pipe). Reading fewer than `count` bytes at a clean end of stream returns a
/// short/empty buffer; the framing layer treats an empty read before any frame byte as
/// an orderly end of stream.
public protocol ByteStream {
    func write(_ bytes: [UInt8]) throws
    /// Read exactly `count` bytes, or fewer only at end of stream.
    func readExactly(_ count: Int) throws -> [UInt8]
}

/// Encode a 4-byte big-endian length prefix for `length`.
func lengthPrefix(_ length: Int) -> [UInt8] {
    let n = UInt32(length)
    return [
        UInt8((n >> 24) & 0xff),
        UInt8((n >> 16) & 0xff),
        UInt8((n >> 8) & 0xff),
        UInt8(n & 0xff),
    ]
}

/// Write a 4-byte big-endian length prefix followed by `bytes` (CSIL stream framing),
/// enforcing the max-frame guard before writing.
public func writeLengthPrefixed(_ stream: ByteStream, _ bytes: [UInt8], maxFrame: Int) throws {
    guard bytes.count <= maxFrame else {
        throw TransportError.frameTooLarge(got: bytes.count, maximum: maxFrame)
    }
    try stream.write(lengthPrefix(bytes.count))
    try stream.write(bytes)
}

/// Read one length-prefixed frame, enforcing the max-frame guard before allocating. It
/// returns nil at a clean end of stream before any byte of a frame.
public func readLengthPrefixed(_ stream: ByteStream, maxFrame: Int) throws -> [UInt8]? {
    let header = try stream.readExactly(4)
    if header.isEmpty {
        // A clean end of stream before any frame byte is an orderly close.
        return nil
    }
    guard header.count == 4 else {
        throw TransportError.carrier("truncated length prefix")
    }
    // Compare as an unsigned value before narrowing to Int so a length >= 0x80000000
    // can never slip past the guard and then over-allocate.
    let length =
        UInt32(header[0]) << 24 | UInt32(header[1]) << 16 | UInt32(header[2]) << 8
        | UInt32(header[3])
    guard UInt64(length) <= UInt64(maxFrame) else {
        throw TransportError.frameTooLarge(got: Int(length), maximum: maxFrame)
    }
    let body = try stream.readExactly(Int(length))
    guard body.count == Int(length) else {
        throw TransportError.carrier("truncated frame body")
    }
    return body
}

/// A `FrameCarrier` over any `ByteStream`, using the canonical 4-byte length-prefix
/// framing. A reference type: it wraps a mutable stream with thread-affinity owned by
/// the host, so it is intentionally not `Sendable`.
public final class StreamCarrier: FrameCarrier {
    private let stream: ByteStream
    private let maxFrame: Int

    public init(stream: ByteStream, maxFrame: Int = maxFrameDefault) {
        self.stream = stream
        self.maxFrame = maxFrame
    }

    public func sendFrame(_ frame: [UInt8]) throws {
        try writeLengthPrefixed(stream, frame, maxFrame: maxFrame)
    }

    public func recvFrame() throws -> [UInt8]? {
        try readLengthPrefixed(stream, maxFrame: maxFrame)
    }
}

/// An in-memory `FrameCarrier` backed by queues of frames — for tests and for driving
/// the codec without a socket.
public final class LoopbackFrameCarrier: FrameCarrier {
    public private(set) var outbound: [[UInt8]] = []
    public private(set) var inbound: [[UInt8]] = []

    public init() {}

    /// Queue a frame that a subsequent `recvFrame` will return.
    public func pushInbound(_ frame: [UInt8]) {
        inbound.append(frame)
    }

    /// Take the next frame that was sent via `sendFrame`, or nil if none.
    public func takeOutbound() -> [UInt8]? {
        guard !outbound.isEmpty else { return nil }
        return outbound.removeFirst()
    }

    public func sendFrame(_ frame: [UInt8]) throws {
        outbound.append(frame)
    }

    public func recvFrame() throws -> [UInt8]? {
        guard !inbound.isEmpty else { return nil }
        return inbound.removeFirst()
    }
}

/// An in-memory `DatagramCarrier` — for tests and codec drives.
public final class LoopbackDatagramCarrier: DatagramCarrier {
    public private(set) var outbound: [[UInt8]] = []
    public private(set) var inbound: [[UInt8]] = []

    public init() {}

    public func pushInbound(_ datagram: [UInt8]) {
        inbound.append(datagram)
    }

    public func takeOutbound() -> [UInt8]? {
        guard !outbound.isEmpty else { return nil }
        return outbound.removeFirst()
    }

    public func sendDatagram(_ datagram: [UInt8]) throws {
        outbound.append(datagram)
    }

    public func recvDatagram() throws -> [UInt8]? {
        guard !inbound.isEmpty else { return nil }
        return inbound.removeFirst()
    }
}

/// An in-memory `ByteStream` pipe (a byte queue) for exercising `StreamCarrier` end to
/// end without a real socket.
public final class InMemoryByteStream: ByteStream {
    private var buffer: [UInt8] = []
    private var position = 0

    public init() {}

    public func write(_ bytes: [UInt8]) throws {
        buffer.append(contentsOf: bytes)
    }

    public func readExactly(_ count: Int) throws -> [UInt8] {
        let available = buffer.count - position
        let take = min(count, available)
        let slice = Array(buffer[position..<(position + take)])
        position += take
        return slice
    }
}
