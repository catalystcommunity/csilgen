// The configurable max-frame guard (conventions doc §5): a host sets the limit up or
// down through the carrier's public API, the limit applies to reads and writes alike,
// an oversized inbound length is rejected before allocation, and an invalid limit
// fails at construction rather than on the first frame.
import XCTest

@testable import CsilgenTransport

/// Counts bytes handed out, so a test can prove the guard fires before the frame body
/// is ever pulled off the wire.
private final class CountingByteStream: ByteStream {
    private var buffer: [UInt8]
    private var position = 0
    private(set) var bytesRead = 0

    init(_ initial: [UInt8] = []) {
        self.buffer = initial
    }

    var written: [UInt8] { buffer }

    func write(_ bytes: [UInt8]) throws {
        buffer.append(contentsOf: bytes)
    }

    func readExactly(_ count: Int) throws -> [UInt8] {
        let available = buffer.count - position
        let take = min(count, available)
        let slice = Array(buffer[position..<(position + take)])
        position += take
        bytesRead += take
        return slice
    }
}

final class MaxFrameTests: XCTestCase {
    func testDefaultLimitAcceptsFrameBelowIt() throws {
        let carrier = try StreamCarrier(stream: InMemoryByteStream())
        let frame = [UInt8](repeating: 0xAB, count: 1024)
        try carrier.sendFrame(frame)
        XCTAssertEqual(try carrier.recvFrame(), frame)
    }

    func testDefaultLimitRejectsFrameAboveIt() throws {
        let stream = CountingByteStream()
        let carrier = try StreamCarrier(stream: stream)
        let frame = [UInt8](repeating: 0, count: maxFrameDefault + 1)
        XCTAssertThrowsError(try carrier.sendFrame(frame)) { err in
            guard case TransportError.frameTooLarge(_, let maximum) = err else {
                return XCTFail("expected frameTooLarge, got \(err)")
            }
            XCTAssertEqual(maximum, maxFrameDefault)
        }
        XCTAssertTrue(stream.written.isEmpty, "a rejected frame must not put bytes on the wire")
    }

    func testLargerCustomLimitAcceptsWhatDefaultRejects() throws {
        let raised = maxFrameDefault + 4096
        let carrier = try StreamCarrier(stream: InMemoryByteStream(), maxFrame: raised)
        let frame = [UInt8](repeating: 0, count: maxFrameDefault + 1)
        try carrier.sendFrame(frame)
        XCTAssertEqual(try carrier.recvFrame()?.count, frame.count)
    }

    func testSmallerCustomLimitRejectsWhatDefaultAccepts() throws {
        let carrier = try StreamCarrier(stream: InMemoryByteStream(), maxFrame: 64)
        let frame = [UInt8](repeating: 0xCD, count: 1024)
        XCTAssertThrowsError(try carrier.sendFrame(frame)) { err in
            guard case TransportError.frameTooLarge = err else {
                return XCTFail("expected frameTooLarge, got \(err)")
            }
        }
    }

    func testOversizedIncomingLengthRejectedBeforeAllocation() throws {
        // A prefix claiming ~4 GiB followed by no body: if the guard ran after the read
        // this would allocate; it must fail on the prefix alone.
        let stream = CountingByteStream([0xFF, 0xFF, 0xFF, 0xFF])
        let carrier = try StreamCarrier(stream: stream, maxFrame: 4096)
        XCTAssertThrowsError(try carrier.recvFrame()) { err in
            guard case TransportError.frameTooLarge = err else {
                return XCTFail("expected frameTooLarge, got \(err)")
            }
        }
        XCTAssertEqual(stream.bytesRead, 4, "guard must fire on the 4-byte prefix alone")
    }

    func testInvalidLimitsRejectedAtConstruction() {
        for limit in [0, -1, -4096, maxFrameLimit + 1, Int.max] {
            XCTAssertThrowsError(
                try StreamCarrier(stream: InMemoryByteStream(), maxFrame: limit),
                "limit \(limit) must be rejected"
            ) { err in
                guard case TransportError.invalidMaxFrame(_, let ceiling) = err else {
                    return XCTFail("expected invalidMaxFrame, got \(err)")
                }
                XCTAssertEqual(ceiling, maxFrameLimit)
            }
        }
    }

    func testBoundaryLimitsAccepted() throws {
        for limit in [1, maxFrameDefault, maxFrameLimit] {
            let carrier = try StreamCarrier(stream: InMemoryByteStream(), maxFrame: limit)
            XCTAssertEqual(carrier.maxFrame, limit)
        }
    }
}
