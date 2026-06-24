// Behavioral tests beyond the byte-exact conformance vectors: client/server dispatch
// over the built-in carriers, stream framing, the sequence tracker's restart/gap
// semantics, canonical map ordering, and the two allocation guards (frame size and the
// negative-integer decode floor).
import XCTest

@testable import CsilgenTransport

final class RoundtripTests: XCTestCase {
    func testServerDispatchesAndReplies() throws {
        let carrier = LoopbackFrameCarrier()
        let req = RpcRequest(service: "World", op: "join", payload: [0xa0], id: 7)
        carrier.pushInbound(req.encode())

        let server = RpcServer(carrier: carrier)
        let served = try server.serveOne { incoming in
            XCTAssertEqual(incoming.service, "World")
            XCTAssertEqual(incoming.op, "join")
            return .reply(variant: "JoinResponse", payload: [0xa0])
        }
        XCTAssertTrue(served)

        let out = carrier.takeOutbound()!
        let resp = try RpcResponse.decode(out)
        XCTAssertEqual(resp.id, 7, "the reply echoes the request correlation id")
        XCTAssertEqual(resp.status, .ok)
        XCTAssertEqual(resp.variant, "JoinResponse")
        XCTAssertEqual(resp.payload, [0xa0])
    }

    func testServerRepliesMalformedOnGarbage() throws {
        let carrier = LoopbackFrameCarrier()
        carrier.pushInbound([0xff, 0xff, 0xff])
        let server = RpcServer(carrier: carrier)
        let served = try server.serveOne { _ in .reply(variant: "X", payload: []) }
        XCTAssertTrue(served)
        let resp = try RpcResponse.decode(carrier.takeOutbound()!)
        XCTAssertEqual(resp.status, .malformedEnvelope)
    }

    func testClientCallEncodesRequestAndSurfacesStatus() throws {
        let carrier = LoopbackFrameCarrier()
        // Pre-queue a non-ok response; the client must surface it as a thrown status.
        carrier.pushInbound(
            RpcResponse.transportError(status: .unknownServiceOrOp, message: "no such op").encode())
        let client = RpcClient(carrier: carrier, multiplexed: true)
        XCTAssertThrowsError(try client.call(service: "World", op: "nope", payload: [0xa0])) { err in
            guard case TransportError.status(_, let code, _) = err else {
                return XCTFail("expected a status error, got \(err)")
            }
            XCTAssertEqual(code, 2)
        }
        // The request it sent must carry a correlation id on a multiplexed carrier.
        let sent = try RpcRequest.decode(carrier.takeOutbound()!)
        XCTAssertEqual(sent.id, 1)
    }

    func testStreamCarrierFramesMultipleMessages() throws {
        let stream = InMemoryByteStream()
        let carrier = StreamCarrier(stream: stream)
        try carrier.sendFrame([1, 2, 3])
        try carrier.sendFrame([])
        try carrier.sendFrame([9])
        XCTAssertEqual(try carrier.recvFrame(), [1, 2, 3])
        XCTAssertEqual(try carrier.recvFrame(), [])
        XCTAssertEqual(try carrier.recvFrame(), [9])
        XCTAssertNil(try carrier.recvFrame(), "a clean end of stream returns nil")
    }

    func testFrameTooLargeGuard() {
        let stream = InMemoryByteStream()
        XCTAssertThrowsError(try writeLengthPrefixed(stream, [0, 0, 0, 0, 0], maxFrame: 4)) { err in
            guard case TransportError.frameTooLarge = err else {
                return XCTFail("expected frameTooLarge, got \(err)")
            }
        }
    }

    func testNegativeIntDecodeFloorGuard() {
        // major type 1 with an 8-byte arg of 0xffff…ffff names a value below Int64.min;
        // it must throw rather than wrap.
        XCTAssertThrowsError(
            try decodeEnvelope([0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]))
    }

    func testCanonicalMapOrderingIsIndependentOfAppendOrder() {
        let a = canonMap([
            CBOREntry(key: .text("service"), value: .text("World")),
            CBOREntry(key: .text("v"), value: .uint(1)),
            CBOREntry(key: .text("op"), value: .text("join")),
        ])
        let b = canonMap([
            CBOREntry(key: .text("op"), value: .text("join")),
            CBOREntry(key: .text("v"), value: .uint(1)),
            CBOREntry(key: .text("service"), value: .text("World")),
        ])
        XCTAssertEqual(encodeValue(a), encodeValue(b))
        // Shorter keys sort first (length is in the head byte): v, op, service.
        guard case .map(let entries) = a else { return XCTFail("not a map") }
        XCTAssertEqual(entries.map { if case .text(let t) = $0.key { return t } else { return "" } },
            ["v", "op", "service"])
    }

    func testSeqTrackerSemantics() {
        let t = SeqTracker()
        assertKind(t.observe(seq: 5, epoch: nil), .first)
        assertKind(t.observe(seq: 6, epoch: nil), .advanced(gap: 0))
        assertKind(t.observe(seq: 9, epoch: nil), .advanced(gap: 2))
        assertKind(t.observe(seq: 7, epoch: nil), .lateOrDuplicate)
        assertKind(t.observe(seq: 0, epoch: nil), .advanced(gap: 0), "seq 0 is unsequenced")
    }

    func testSeqTrackerRestartOnlyAfterPriorEpoch() {
        let t = SeqTracker()
        // First epoch ever is NOT a restart.
        assertKind(t.observe(seq: 1, epoch: 0), .first)
        assertKind(t.observe(seq: 2, epoch: 0), .advanced(gap: 0))
        // A changed epoch is a restart.
        assertKind(t.observe(seq: 1, epoch: 1), .restart)
    }

    private func assertKind(
        _ got: SeqEventKind, _ want: SeqEventKind, _ note: String = "",
        file: StaticString = #filePath, line: UInt = #line
    ) {
        XCTAssertEqual(got, want, note, file: file, line: line)
    }
}
