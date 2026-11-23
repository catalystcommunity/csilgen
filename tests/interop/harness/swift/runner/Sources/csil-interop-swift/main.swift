// Swift interop harness. It mirrors the Rust reference (tests/interop/harness/rust).
//
// It runs as a server (binds a loopback port and serves one transport) or as a
// client (connects, runs the case battery for one transport, and prints one JSON
// result line). See tests/interop/README.md for the contract.

#if canImport(Glibc)
import Glibc
#else
import Darwin
#endif

import CsilgenTransport
import InteropApi

// The wire carries the CSIL service name verbatim as written in interop.csil.
let service = "Interop"
let ticks: UInt64 = 3

// ---------------------------------------------------------------------------
// Fixed language-neutral test vectors (see README "Fixed test vectors").
// ---------------------------------------------------------------------------

func scalarsOk() -> Scalars {
    Scalars(
        i: -42,
        u: 42,
        n: -7,
        f: 3.5,
        t: "héllo 世界",
        raw: [0x01, 0x02, 0xf0, 0xff],
        flag: true,
        due: "2000-02-29",
        when: "2026-06-29T12:34:56Z",
        // 1782736496.5 on the wire: tag 1 around a float.
        seen: "2026-06-29T12:34:56.500Z",
        amount: "123.45",
        // A value equal to a literal arm wires as [1,"pending"]; one with no
        // literal match wires as [0,...].
        statusLiteral: .case1("pending"),
        statusFree: .text("unlisted"),
        note: .case1("info"),
        size: .medium,
        level: .high,
        season: .autumn,
        shipText: .ground,
        shipInt: .v2
    )
}

func collectionsOk() -> Collections {
    Collections(
        names: ["a", "b"],
        atLeastOne: [1, 2, 3],
        bounded: [10, 20],
        exact3: [7, 8, 9],
        scores: ["x": 1, "y": 2],
        color: .green,
        tone: .blue,
        prio: .v2,
        who: .uint(4242),
        pair: (field0: "p", field1: 5),
        triple: (field0: "t", field1: 9, field2: true),
        extra: ["k": .text("v")]
    )
}

func constrainedOk() -> Constrained {
    Constrained(code: "PRD-AB12CD", qty: 10, rate: "0.25", password: "hunter2hunter2", tags: ["one", "two"])
}

func constrainedBad() -> Constrained {
    Constrained(code: "bad", qty: 0, rate: "9.9", password: "x", tags: [])
}

func nestedOk() -> Nested {
    Nested(inner: scalarsOk(), maybe: constrainedOk(), many: [scalarsOk()])
}

// ---------------------------------------------------------------------------
// Result accumulation and JSON output.
// ---------------------------------------------------------------------------

final class Cases {
    private let transport: String
    private var out: [(name: String, ok: Bool, detail: String)] = []

    init(_ transport: String) {
        self.transport = transport
    }

    func pass(_ name: String) {
        out.append((name, true, ""))
    }

    func fail(_ name: String, _ detail: String) {
        out.append((name, false, detail))
    }

    func check(_ name: String, _ condition: Bool, _ detail: @autoclosure () -> String) {
        if condition { pass(name) } else { fail(name, detail()) }
    }

    func emit() {
        var s = "{\"lang\":\"swift\",\"transport\":\"\(transport)\",\"cases\":["
        for (index, item) in out.enumerated() {
            if index > 0 { s += "," }
            s += "{\"name\":\"\(item.name)\",\"ok\":\(item.ok),\"detail\":\(jsonString(item.detail))}"
        }
        s += "]}"
        print(s)
        fflush(stdout)
    }
}

func jsonString(_ s: String) -> String {
    var out = "\""
    for scalar in s.unicodeScalars {
        switch scalar {
        case "\"": out += "\\\""
        case "\\": out += "\\\\"
        case "\n": out += "\\n"
        case "\t": out += "\\t"
        case "\r": out += "\\r"
        default:
            if scalar.value < 0x20 {
                let hex = String(scalar.value, radix: 16)
                out += "\\u" + String(repeating: "0", count: 4 - hex.count) + hex
            } else {
                out.unicodeScalars.append(scalar)
            }
        }
    }
    return out + "\""
}

// ---------------------------------------------------------------------------
// Server-side handler implementing the generated protocol.
// ---------------------------------------------------------------------------

struct Handlers: Interop {
    func echoScalars(_ request: Scalars) throws -> Scalars { request }
    func echoCollections(_ request: Collections) throws -> Collections { request }
    func echoNested(_ request: Nested) throws -> EchoNestedResult { EchoNestedResult(ok: true, echo: request) }
    func echoOptBytes(_ request: OptBytes) throws -> OptBytes { request }
    func duplex(_ msg: Scalars) throws {}

    func validateConstrained(_ request: Constrained) throws -> Constrained {
        do {
            try request.validate()
        } catch let error as CsilValidationError {
            // The typed error arm rides as a status-0 `ServiceError` variant.
            throw ServiceErrorRefusal(value: ServiceError(code: 422, message: error.message))
        }
        return request
    }
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

/// Hands the lib client's reply to the typed client. The generated client selects
/// the reply arm from the variant, so the carrier only passes it on.
struct RpcTransport: CsilTransport {
    let client: RpcClient

    func call(service: String, op: String, request: [UInt8]) throws -> [UInt8] {
        try client.call(service: service, op: op, payload: request).payload
    }

    func callReply(service: String, op: String, request: [UInt8]) throws -> (variant: String?, payload: [UInt8]) {
        let response = try client.call(service: service, op: op, payload: request)
        return (response.variant, response.payload)
    }
}

/// Decodes the request (a decode failure is a malformed envelope, as in the
/// reference) and then encodes the reply. A reply that cannot be encoded is a
/// server failure, so it goes back as the internal status.
func serve<T>(
    _ payload: [UInt8],
    _ decode: ([UInt8]) throws -> T,
    _ reply: (T) throws -> (variant: String, payload: [UInt8])
) -> HandlerOutcome {
    let value: T
    do {
        value = try decode(payload)
    } catch {
        return .transportError(status: .malformedEnvelope, message: "\(error)")
    }
    do {
        let (variant, bytes) = try reply(value)
        return .reply(variant: variant, payload: bytes)
    } catch {
        return .transportError(status: .internal, message: "\(error)")
    }
}

func dispatch(_ handlers: Handlers, _ request: RpcRequest) -> HandlerOutcome {
    switch request.op {
    case "echo-scalars":
        return serve(request.payload, Scalars.fromCbor) { ("Scalars", try handlers.echoScalars($0).toCbor()) }
    case "echo-collections":
        return serve(request.payload, Collections.fromCbor) {
            ("Collections", try handlers.echoCollections($0).toCbor())
        }
    case "echo-nested":
        return serve(request.payload, Nested.fromCbor) {
            ("EchoNestedResult", try handlers.echoNested($0).toCbor())
        }
    case "echo-opt-bytes":
        return serve(request.payload, OptBytes.fromCbor) { ("OptBytes", try handlers.echoOptBytes($0).toCbor()) }
    case "validate-constrained":
        return serve(request.payload, Constrained.fromCbor) { input in
            try encodeInteropValidateConstrainedReply { try handlers.validateConstrained(input) }
        }
    default:
        return .transportError(status: .unknownServiceOrOp, message: "unknown op \(request.op)")
    }
}

func rpcServer(_ listener: TcpListener) {
    let handlers = Handlers()
    while let stream = listener.accept() {
        defer { stream.close() }
        guard let carrier = try? StreamCarrier(stream: stream) else { continue }
        let server = RpcServer(carrier: carrier)
        // Serve until the peer closes the connection or drops it.
        while (try? server.serveOne({ dispatch(handlers, $0) })) == true {}
    }
}

func rpcClient(_ stream: SocketStream) throws -> Cases {
    let cases = Cases("rpc")
    let client = InteropClient(
        transport: RpcTransport(client: RpcClient(carrier: try StreamCarrier(stream: stream), multiplexed: false)))

    do {
        let r = try client.echoScalars(scalarsOk())
        cases.check("echo-scalars/success", r == scalarsOk(), "mismatch: \(r)")
    } catch {
        cases.fail("echo-scalars/success", "\(error)")
    }
    do {
        let r = try client.echoCollections(collectionsOk())
        cases.check("echo-collections/success", r == collectionsOk(), "mismatch: \(r)")
    } catch {
        cases.fail("echo-collections/success", "\(error)")
    }
    do {
        let r = try client.echoNested(nestedOk())
        cases.check("echo-nested/success", r.ok && r.echo == nestedOk(), "mismatch: \(r)")
    } catch {
        cases.fail("echo-nested/success", "\(error)")
    }
    do {
        let r = try client.validateConstrained(constrainedOk())
        cases.check("validate-constrained/success", r == constrainedOk(), "mismatch: \(r)")
    } catch {
        cases.fail("validate-constrained/success", "\(error)")
    }
    do {
        _ = try client.validateConstrained(constrainedBad())
        cases.fail("validate-constrained/failure", "server accepted invalid input")
    } catch let refusal as ServiceErrorRefusal {
        // The typed error arm must surface as a service error, not a transport error.
        cases.check("validate-constrained/failure", refusal.value.code == 422, "wrong code \(refusal.value.code)")
    } catch {
        cases.fail("validate-constrained/failure", "expected service error, got \(error)")
    }

    // Absent, present-empty, and present-non-empty are three distinct states that
    // must each survive the round trip. `nil` is the absent marker; an empty array
    // is present and must not come back as nil.
    do {
        let r = try client.echoOptBytes(OptBytes(tag: "absent", payload: nil))
        cases.check("opt-bytes/absent", r.payload == nil, "expected absent, got \(String(describing: r.payload))")
    } catch {
        cases.fail("opt-bytes/absent", "\(error)")
    }
    do {
        let r = try client.echoOptBytes(OptBytes(tag: "empty", payload: []))
        cases.check(
            "opt-bytes/present-empty", r.payload == [],
            "expected present-and-empty, got \(String(describing: r.payload))")
    } catch {
        cases.fail("opt-bytes/present-empty", "\(error)")
    }
    do {
        let sent: [UInt8] = [0x01, 0x02, 0xf0, 0xff]
        let r = try client.echoOptBytes(OptBytes(tag: "full", payload: sent))
        cases.check(
            "opt-bytes/present-non-empty", r.payload == sent,
            "expected 0102f0ff, got \(String(describing: r.payload))")
    } catch {
        cases.fail("opt-bytes/present-non-empty", "\(error)")
    }
    return cases
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

func sendEvent(_ carrier: StreamCarrier, _ event: Event) throws {
    try carrier.sendFrame(try event.encode(.verbose))
}

func eventsServer(_ listener: TcpListener) {
    while let stream = listener.accept() {
        defer { stream.close() }
        do {
            let carrier = try StreamCarrier(stream: stream)
            // Handshake: read $hello, reply $hello-ack (verbose).
            guard let helloFrame = try carrier.recvFrame() else { continue }
            let hello = try Event.decode(helloFrame, .verbose)
            guard hello.event == Control.helloName else { continue }
            _ = try Hello.decode(hello.payload)
            let ack = HelloAck(v: 1, profile: "verbose")
            try sendEvent(carrier, .verbose(service: nil, event: Control.helloAckName, payload: ack.encode()))

            // Push N ticks (on-tick / server push).
            for seq in 0..<ticks {
                try sendEvent(carrier, .verbose(service: service, event: "on-tick", payload: try Tick(seq: seq).toCbor()))
            }

            // React loop.
            react: while let frame = try carrier.recvFrame() {
                let event = try Event.decode(frame, .verbose)
                switch event.event {
                case Control.pingName:
                    try sendEvent(carrier, .verbose(service: nil, event: Control.pongName, payload: event.payload))
                case Control.closeName:
                    break react
                case "duplex":
                    let scalars = try Scalars.fromCbor(event.payload)
                    try Handlers().duplex(scalars)
                    try sendEvent(carrier, .verbose(service: service, event: "duplex", payload: try scalars.toCbor()))
                default:
                    try sendEvent(carrier, .verbose(service: nil, event: Control.errorName, payload: []))
                }
            }
        } catch {
            // The peer dropped; accept the next connection.
        }
    }
}

func eventName(_ frame: [UInt8]?) -> String? {
    guard let frame, let event = try? Event.decode(frame, .verbose) else { return nil }
    return event.event
}

func eventsClient(_ stream: SocketStream) throws -> Cases {
    let cases = Cases("events")
    let carrier = try StreamCarrier(stream: stream)

    // Handshake.
    let hello = Hello(versions: [1], profiles: ["verbose"], service: service)
    try sendEvent(carrier, .verbose(service: nil, event: Control.helloName, payload: hello.encode()))
    cases.check("events/handshake", eventName(try carrier.recvFrame()) == Control.helloAckName, "no $hello-ack")

    // on-tick: expect TICKS pushes with seq 0..TICKS.
    var tickOk = true
    var detail = ""
    for expect in 0..<ticks {
        guard let frame = try carrier.recvFrame() else {
            tickOk = false
            detail = "stream closed during ticks"
            break
        }
        do {
            let event = try Event.decode(frame, .verbose)
            guard event.event == "on-tick" else {
                tickOk = false
                detail = "expected on-tick got \(String(describing: event.event))"
                break
            }
            let tick = try Tick.fromCbor(event.payload)
            if tick.seq != expect {
                tickOk = false
                detail = "tick seq \(tick.seq) != \(expect)"
            }
        } catch {
            tickOk = false
            detail = "decode: \(error)"
        }
    }
    cases.check("on-tick/success", tickOk, detail)

    // duplex: send Scalars, expect the echo.
    try sendEvent(carrier, .verbose(service: service, event: "duplex", payload: try scalarsOk().toCbor()))
    if let frame = try carrier.recvFrame() {
        do {
            let event = try Event.decode(frame, .verbose)
            if event.event == "duplex" {
                let echoed = try Scalars.fromCbor(event.payload)
                cases.check("duplex/success", echoed == scalarsOk(), "echo mismatch")
            } else {
                cases.fail("duplex/success", "got \(String(describing: event.event))")
            }
        } catch {
            cases.fail("duplex/success", "\(error)")
        }
    } else {
        cases.fail("duplex/success", "no echo")
    }

    // unknown-method: send a bogus channel method, expect $error.
    try sendEvent(carrier, .verbose(service: service, event: "Bogus", payload: []))
    cases.check("unknown-method/failure", eventName(try carrier.recvFrame()) == Control.errorName, "expected $error")

    try sendEvent(carrier, .verbose(service: nil, event: Control.closeName, payload: []))
    return cases
}

// ---------------------------------------------------------------------------
// Datagrams
// ---------------------------------------------------------------------------

let opEchoScalars: UInt64 = 0
let opEchoCollections: UInt64 = 1
let opErrorSentinel: UInt64 = 255

func datagramServer(_ socket: UdpSocket) {
    while true {
        guard let (bytes, source, length) = socket.receiveFrom() else { continue }
        guard let datagram = try? Datagram.decode(bytes) else { continue }
        var reply = Datagram(opOrd: opErrorSentinel, seq: datagram.seq, payload: [])
        switch datagram.opOrd {
        case opEchoScalars:
            if let echoed = try? Scalars.fromCbor(datagram.payload).toCbor() {
                reply = Datagram(opOrd: opEchoScalars, seq: datagram.seq, payload: echoed)
            }
        case opEchoCollections:
            if let echoed = try? Collections.fromCbor(datagram.payload).toCbor() {
                reply = Datagram(opOrd: opEchoCollections, seq: datagram.seq, payload: echoed)
            }
        default:
            break
        }
        socket.send(reply.encode(), to: source, length: length)
    }
}

func datagramClient(_ socket: UdpSocket) -> Cases {
    let cases = Cases("datagrams")
    socket.setReceiveTimeout(seconds: 3)

    func roundtrip(_ op: UInt64, _ seq: UInt64, _ payload: [UInt8]) throws -> Datagram {
        try socket.send(Datagram(opOrd: op, seq: seq, payload: payload).encode())
        return try Datagram.decode(try socket.receive())
    }

    do {
        let d = try roundtrip(opEchoScalars, 1, try scalarsOk().toCbor())
        if d.opOrd == opEchoScalars {
            cases.check("echo-scalars/success", try Scalars.fromCbor(d.payload) == scalarsOk(), "payload mismatch")
        } else {
            cases.fail("echo-scalars/success", "op_ord \(d.opOrd)")
        }
    } catch {
        cases.fail("echo-scalars/success", "\(error)")
    }
    do {
        let d = try roundtrip(opEchoCollections, 2, try collectionsOk().toCbor())
        if d.opOrd == opEchoCollections {
            cases.check(
                "echo-collections/success", try Collections.fromCbor(d.payload) == collectionsOk(), "payload mismatch")
        } else {
            cases.fail("echo-collections/success", "op_ord \(d.opOrd)")
        }
    } catch {
        cases.fail("echo-collections/success", "\(error)")
    }
    do {
        let d = try roundtrip(99, 3, [])
        cases.check(
            "bad-op-ord/failure", d.opOrd == opErrorSentinel, "expected error sentinel, got op_ord \(d.opOrd)")
    } catch {
        cases.fail("bad-op-ord/failure", "\(error)")
    }
    return cases
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

func ready() {
    print("READY")
    fflush(stdout)
}

func usage() -> Never {
    fputs("usage: csil-interop-swift <server|client> <rpc|events|datagrams> <port>\n", stderr)
    exit(2)
}

let arguments = CommandLine.arguments
guard arguments.count >= 4, let port = UInt16(arguments[3]) else { usage() }
let mode = arguments[1]
let transport = arguments[2]
guard ["rpc", "events", "datagrams"].contains(transport) else { usage() }
ignoreBrokenPipe()

do {
    switch (mode, transport) {
    case ("server", "datagrams"):
        let socket = try UdpSocket(bindPort: port)
        ready()
        datagramServer(socket)
    case ("server", _):
        let listener = try TcpListener(port: port)
        ready()
        if transport == "rpc" { rpcServer(listener) } else { eventsServer(listener) }
    case ("client", "datagrams"):
        // Bind an ephemeral local UDP port; the server replies to this source.
        let socket = try UdpSocket(bindPort: 0)
        try socket.connect(port: port)
        datagramClient(socket).emit()
    case ("client", _):
        let stream = try connectTcp(port: port)
        let cases = transport == "rpc" ? try rpcClient(stream) : try eventsClient(stream)
        cases.emit()
        stream.close()
    default:
        usage()
    }
} catch {
    fputs("csil-interop-swift: \(error)\n", stderr)
    exit(1)
}
