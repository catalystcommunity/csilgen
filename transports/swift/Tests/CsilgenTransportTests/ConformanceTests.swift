// Verify the Swift reference library against the checked-in conformance vectors
// (transports/conformance/*.json). This both guards the encoders/decoders against drift
// and demonstrates the contract every reference library follows: reconstruct each
// vector's envelope from its language-neutral `input`, assert encode → `hex`, and assert
// decode(`hex`) → the same envelope. Ported from transports/go/conformance_test.go.
//
// Foundation is imported only here, in the test target, for JSONSerialization + path
// math — the library itself stays Foundation-free so the wire codec is portable.
import Foundation
import XCTest

@testable import CsilgenTransport

final class ConformanceTests: XCTestCase {
    func testRpcVectors() throws {
        for vec in try loadVectors("rpc.json") {
            let input = vec.input
            let actual: [UInt8]
            switch try reqStr(input, "kind") {
            case "request":
                var r = RpcRequest(
                    service: try reqStr(input, "service"),
                    op: try reqStr(input, "op"),
                    payload: try reqHex(input, "payload_hex"))
                r.id = optU64(input, "id")
                r.auth = optStr(input, "auth")
                XCTAssertEqual(try RpcRequest.decode(r.encode()), r, "\(vec.name) decode")
                actual = r.encode()
            case "response":
                let r = RpcResponse(
                    status: Status(code: try reqInt(input, "status")),
                    payload: try reqHex(input, "payload_hex"),
                    id: optU64(input, "id"),
                    variant: optStr(input, "variant"),
                    error: optStr(input, "error"))
                XCTAssertEqual(try RpcResponse.decode(r.encode()), r, "\(vec.name) decode")
                actual = r.encode()
            case "push":
                let p = RpcPush(
                    service: try reqStr(input, "service"),
                    event: try reqStr(input, "event"),
                    payload: try reqHex(input, "payload_hex"))
                XCTAssertEqual(try RpcPush.decode(p.encode()), p, "\(vec.name) decode")
                actual = p.encode()
            default:
                XCTFail("unknown rpc kind in \(vec.name)")
                continue
            }
            XCTAssertEqual(toHex(actual), vec.hex, "\(vec.name) encode")
        }
    }

    func testEventsVectors() throws {
        for vec in try loadVectors("events.json") {
            let input = vec.input
            // Control payloads are keyed by "control"; application events by "profile".
            if let control = optStr(input, "control") {
                let bytes: [UInt8]
                switch control {
                case "hello":
                    bytes = Hello(
                        versions: try uintArray(input, "versions"),
                        profiles: try stringArray(input, "profiles"),
                        service: optStr(input, "service"),
                        auth: optStr(input, "auth")
                    ).encode()
                case "hello_ack":
                    bytes = HelloAck(
                        v: try reqU64(input, "v"),
                        profile: try reqStr(input, "profile"),
                        session: optStr(input, "session")
                    ).encode()
                case "ping":
                    bytes = Heartbeat(nonce: try reqU64(input, "nonce"), at: optU64(input, "at"))
                        .encode()
                case "close":
                    bytes = Close(
                        status: Status(code: try reqInt(input, "status")),
                        reason: optStr(input, "reason")
                    ).encode()
                default:
                    XCTFail("unknown control \(control)")
                    continue
                }
                XCTAssertEqual(toHex(bytes), vec.hex, "\(vec.name) encode")
                continue
            }

            guard let profile = Profile.parse(try reqStr(input, "profile")) else {
                XCTFail("unknown profile in \(vec.name)")
                continue
            }
            let payload = try reqHex(input, "payload_hex")
            var ev: Event
            switch profile {
            case .verbose:
                ev = Event.verbose(
                    service: optStr(input, "service"), event: try reqStr(input, "event"),
                    payload: payload)
                ev.id = optU64(input, "id")
            case .compact:
                ev = Event.compact(
                    serviceOrd: try reqU64(input, "service_ord"),
                    opOrd: try reqU64(input, "op_ord"), payload: payload)
                ev.id = optU64(input, "id")
            }
            XCTAssertEqual(toHex(try ev.encode(profile)), vec.hex, "\(vec.name) encode")
            XCTAssertEqual(try Event.decode(ev.encode(profile), profile), ev, "\(vec.name) decode")
        }
    }

    func testDatagramVectors() throws {
        for vec in try loadVectors("datagrams.json") {
            let input = vec.input
            switch try reqStr(input, "profile") {
            case "cbor-array":
                let d = Datagram(
                    opOrd: try reqU64(input, "op_ord"), seq: try reqU64(input, "seq"),
                    payload: try reqHex(input, "payload_hex"))
                XCTAssertEqual(toHex(d.encode()), vec.hex, "\(vec.name) encode")
                XCTAssertEqual(try Datagram.decode(d.encode()), d, "\(vec.name) decode")
            case "compact-header":
                var d = CompactDatagram(
                    opOrd: UInt8(try reqU64(input, "op_ord")),
                    seq: UInt16(try reqU64(input, "seq")),
                    body: try reqHex(input, "body_hex"))
                if let e = optU64(input, "epoch") { d = d.withEpoch(UInt8(e)) }
                XCTAssertEqual(toHex(d.encode()), vec.hex, "\(vec.name) encode")
                XCTAssertEqual(try CompactDatagram.decode(d.encode()), d, "\(vec.name) decode")
            default:
                XCTFail("unknown datagram profile in \(vec.name)")
            }
        }
    }
}

// MARK: - Vector loading + language-neutral input helpers

struct Vector {
    let name: String
    let hex: String
    let input: [String: Any]
}

/// The conformance vectors live at <repo>/transports/conformance, outside this package.
/// Walk up from this test source file rather than depend on the working directory.
private func conformanceDir() -> URL {
    var url = URL(fileURLWithPath: #filePath)
    // .../transports/swift/Tests/CsilgenTransportTests/ConformanceTests.swift → up to transports
    for _ in 0..<4 { url.deleteLastPathComponent() }
    return url.appendingPathComponent("conformance")
}

func loadVectors(_ file: String) throws -> [Vector] {
    let path = conformanceDir().appendingPathComponent(file)
    let data = try Data(contentsOf: path)
    // Cast element-by-element rather than to `[[String: Any]]` in one shot: Linux's
    // swift-corelibs-foundation is stricter about bridging nested heterogeneous arrays.
    guard let doc = try JSONSerialization.jsonObject(with: data) as? [String: Any],
        let raw = doc["vectors"] as? [Any]
    else {
        throw TransportError.malformed("vectors file \(file) is not the expected shape")
    }
    return raw.compactMap { entry in
        guard let e = entry as? [String: Any],
            let name = e["name"] as? String, let hex = e["hex"] as? String,
            let input = e["input"] as? [String: Any]
        else { return nil }
        return Vector(name: name, hex: hex, input: input)
    }
}

func toHex(_ bytes: [UInt8]) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}

func fromHex(_ s: String) -> [UInt8] {
    var out: [UInt8] = []
    out.reserveCapacity(s.count / 2)
    var it = s.startIndex
    while it < s.endIndex {
        let next = s.index(it, offsetBy: 2)
        out.append(UInt8(s[it..<next], radix: 16)!)
        it = next
    }
    return out
}

private func numberValue(_ any: Any?) -> Int64? {
    switch any {
    case let n as NSNumber: return n.int64Value
    case let i as Int: return Int64(i)
    case let d as Double: return Int64(d)
    default: return nil
    }
}

func optStr(_ m: [String: Any], _ k: String) -> String? {
    guard let v = m[k], !(v is NSNull) else { return nil }
    return v as? String
}

func optU64(_ m: [String: Any], _ k: String) -> UInt64? {
    guard let v = m[k], !(v is NSNull), let n = numberValue(v) else { return nil }
    return UInt64(n)
}

func reqStr(_ m: [String: Any], _ k: String) throws -> String {
    guard let s = optStr(m, k) else { throw TransportError.malformed("missing string \(k)") }
    return s
}

func reqU64(_ m: [String: Any], _ k: String) throws -> UInt64 {
    guard let n = optU64(m, k) else { throw TransportError.malformed("missing integer \(k)") }
    return n
}

func reqInt(_ m: [String: Any], _ k: String) throws -> Int {
    guard let v = m[k], let n = numberValue(v) else {
        throw TransportError.malformed("missing integer \(k)")
    }
    return Int(n)
}

func reqHex(_ m: [String: Any], _ k: String) throws -> [UInt8] {
    fromHex(try reqStr(m, k))
}

func uintArray(_ m: [String: Any], _ k: String) throws -> [UInt64] {
    guard let arr = m[k] as? [Any] else { throw TransportError.malformed("missing array \(k)") }
    return arr.compactMap { numberValue($0).map(UInt64.init) }
}

func stringArray(_ m: [String: Any], _ k: String) throws -> [String] {
    guard let arr = m[k] as? [Any] else { throw TransportError.malformed("missing array \(k)") }
    return arr.compactMap { $0 as? String }
}
