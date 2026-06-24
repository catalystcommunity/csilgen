// CSIL-RPC transport — request/response/push envelopes — see csil-rpc-transport.md.

/// A CSIL-RPC request (client -> server).
public struct RpcRequest: Equatable, Sendable {
    public var service: String
    public var op: String
    public var id: UInt64?
    /// The opaque CBOR(request type) bytes (wrapped in tag 24 on the wire).
    public var payload: [UInt8]
    public var auth: String?

    public init(service: String, op: String, payload: [UInt8], id: UInt64? = nil, auth: String? = nil)
    {
        self.service = service
        self.op = op
        self.payload = payload
        self.id = id
        self.auth = auth
    }

    /// Set the correlation id (required on multiplexed carriers).
    public func withID(_ id: UInt64) -> RpcRequest {
        var copy = self
        copy.id = id
        return copy
    }

    /// Set the per-request credential for caller-scoped operations.
    public func withAuth(_ auth: String) -> RpcRequest {
        var copy = self
        copy.auth = auth
        return copy
    }

    public func encode() -> [UInt8] {
        var entries: [CBOREntry] = [
            CBOREntry(key: .text("v"), value: .uint(csilVersion)),
            CBOREntry(key: .text("service"), value: .text(service)),
            CBOREntry(key: .text("op"), value: .text(op)),
            CBOREntry(key: .text("payload"), value: tag24(payload)),
        ]
        if let id { entries.append(CBOREntry(key: .text("id"), value: .uint(id))) }
        if let auth { entries.append(CBOREntry(key: .text("auth"), value: .text(auth))) }
        return encodeValue(canonMap(entries))
    }

    public static func decode(_ b: [UInt8]) throws -> RpcRequest {
        let v = try decodeEnvelope(b)
        try checkVersion(try getUint(v, "v"))
        guard let p = mapGet(v, "payload") else { throw malformed("missing 'payload'") }
        return RpcRequest(
            service: try getText(v, "service"),
            op: try getText(v, "op"),
            payload: try untag24(p),
            id: getUintOpt(v, "id"),
            auth: getTextOpt(v, "auth")
        )
    }
}

/// A CSIL-RPC response (server -> client).
public struct RpcResponse: Equatable, Sendable {
    public var id: UInt64?
    public var status: Status
    /// Names which declared output-choice arm `payload` decodes to (the CSIL type name).
    public var variant: String?
    public var error: String?
    /// The opaque CBOR(output type) bytes; an empty *present* byte string when status
    /// is non-zero (an empty payload is never an absent field).
    public var payload: [UInt8]

    public init(
        status: Status, payload: [UInt8], id: UInt64? = nil, variant: String? = nil,
        error: String? = nil
    ) {
        self.status = status
        self.payload = payload
        self.id = id
        self.variant = variant
        self.error = error
    }

    /// A successful (status ok) typed reply.
    public static func ok(variant: String, payload: [UInt8]) -> RpcResponse {
        RpcResponse(status: .ok, payload: payload, variant: variant)
    }

    /// A transport-level failure (no typed payload).
    public static func transportError(status: Status, message: String) -> RpcResponse {
        RpcResponse(status: status, payload: [], error: message)
    }

    public func withID(_ id: UInt64?) -> RpcResponse {
        var copy = self
        copy.id = id
        return copy
    }

    public func encode() -> [UInt8] {
        var entries: [CBOREntry] = [
            CBOREntry(key: .text("v"), value: .uint(csilVersion)),
            CBOREntry(key: .text("status"), value: .int(Int64(status.code))),
            CBOREntry(key: .text("payload"), value: tag24(payload)),
        ]
        if let id { entries.append(CBOREntry(key: .text("id"), value: .uint(id))) }
        if let variant { entries.append(CBOREntry(key: .text("variant"), value: .text(variant))) }
        if let error { entries.append(CBOREntry(key: .text("error"), value: .text(error))) }
        return encodeValue(canonMap(entries))
    }

    public static func decode(_ b: [UInt8]) throws -> RpcResponse {
        let v = try decodeEnvelope(b)
        try checkVersion(try getUint(v, "v"))
        // payload is present but may wrap an empty byte string on error.
        var payload: [UInt8] = []
        if let p = mapGet(v, "payload") {
            payload = try untag24(p)
        }
        return RpcResponse(
            status: Status(code: Int(try getInt(v, "status"))),
            payload: payload,
            id: getUintOpt(v, "id"),
            variant: getTextOpt(v, "variant"),
            error: getTextOpt(v, "error")
        )
    }

    /// A `TransportError` for a non-ok response, or nil when the response carries a
    /// typed reply (status 0).
    public func asTransportError() -> TransportError? {
        if status.isOk { return nil }
        return TransportError.status(name: status.name, code: status.code, message: error ?? "")
    }
}

/// A CSIL-RPC server push (server -> client) for `<-` operations.
public struct RpcPush: Equatable, Sendable {
    public var service: String
    public var event: String
    public var payload: [UInt8]

    public init(service: String, event: String, payload: [UInt8]) {
        self.service = service
        self.event = event
        self.payload = payload
    }

    public func encode() -> [UInt8] {
        let entries: [CBOREntry] = [
            CBOREntry(key: .text("v"), value: .uint(csilVersion)),
            CBOREntry(key: .text("service"), value: .text(service)),
            CBOREntry(key: .text("event"), value: .text(event)),
            CBOREntry(key: .text("payload"), value: tag24(payload)),
        ]
        return encodeValue(canonMap(entries))
    }

    public static func decode(_ b: [UInt8]) throws -> RpcPush {
        let v = try decodeEnvelope(b)
        try checkVersion(try getUint(v, "v"))
        guard let p = mapGet(v, "payload") else { throw malformed("missing 'payload'") }
        return RpcPush(
            service: try getText(v, "service"),
            event: try getText(v, "event"),
            payload: try untag24(p)
        )
    }
}

/// What a server handler returns for one request: a typed reply on success, or a
/// transport status on failure. An exhaustive `switch` makes adding a status/variant a
/// compile-time fan-out, not a runtime surprise.
public enum HandlerOutcome {
    case reply(variant: String, payload: [UInt8])
    case transportError(status: Status, message: String)
}

/// A CSIL-RPC client over a frame carrier. The carrier is injected (bring your own);
/// the client owns the envelope and a per-connection monotonic correlation id.
public final class RpcClient {
    private let carrier: FrameCarrier
    private let multiplexed: Bool
    private var nextID: UInt64 = 1

    /// `multiplexed` true assigns a correlation id to every request (required on WS /
    /// pipelined streams); false omits it (one-in-flight carriers such as HTTP).
    public init(carrier: FrameCarrier, multiplexed: Bool) {
        self.carrier = carrier
        self.multiplexed = multiplexed
    }

    /// Invoke service/op with an encoded request payload, returning the decoded
    /// response. A non-zero transport status is surfaced as a thrown `TransportError`.
    public func call(service: String, op: String, payload: [UInt8], auth: String? = nil) throws
        -> RpcResponse
    {
        var req = RpcRequest(service: service, op: op, payload: payload, auth: auth)
        if multiplexed {
            req.id = nextID
            nextID += 1
        }
        try carrier.sendFrame(req.encode())
        guard let inbound = try carrier.recvFrame() else {
            throw TransportError.carrier("connection closed before response")
        }
        let resp = try RpcResponse.decode(inbound)
        if let err = resp.asTransportError() {
            throw err
        }
        return resp
    }
}

/// A CSIL-RPC server over a frame carrier. The host supplies a handler mapping a
/// request to an outcome; the generated router is the natural implementation of that
/// handler.
public final class RpcServer {
    private let carrier: FrameCarrier

    public init(carrier: FrameCarrier) {
        self.carrier = carrier
    }

    /// Read one request, dispatch it through `handler`, and write the response. Returns
    /// false at a clean end of stream.
    @discardableResult
    public func serveOne(_ handler: (RpcRequest) -> HandlerOutcome) throws -> Bool {
        guard let frame = try carrier.recvFrame() else {
            return false
        }
        let resp: RpcResponse
        if let req = try? RpcRequest.decode(frame) {
            switch handler(req) {
            case .reply(let variant, let payload):
                resp = RpcResponse.ok(variant: variant, payload: payload).withID(req.id)
            case .transportError(let status, let message):
                resp = RpcResponse.transportError(status: status, message: message).withID(req.id)
            }
        } else {
            resp = RpcResponse.transportError(
                status: .malformedEnvelope, message: "malformed request envelope")
        }
        try carrier.sendFrame(resp.encode())
        return true
    }
}
