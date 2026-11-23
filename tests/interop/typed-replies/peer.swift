precondition(RpErrorCode.accountRateLimited.wireNumber == 42)
precondition(RpErrorCode(wireNumber: 42) == .accountRateLimited)
precondition(RpErrorCode(wireNumber: 1) == nil)

struct Handler: Rp {
    func decryptToken(_ request: Req) throws -> Resp {
        if request.a == "limit" {
            throw RpErrorRefusal(value: RpError(code: .applicationUserLimit, message: "limit"))
        }
        return Resp(b: "ok")
    }
}

// Answers like a server: the variant travels beside the payload, and the request
// text can force a missing or unknown variant.
func answer(service: String, op: String, request: [UInt8]) throws -> (variant: String?, payload: [UInt8]) {
    precondition(service == "Rp" && op == "decrypt-token")
    let req = try Req.fromCbor(request)
    if req.a == "unknown" { throw CsilClientError(code: 2, message: "unknown") }
    let reply = try encodeRpDecryptTokenReply { try Handler().decryptToken(req) }
    if reply.variant == "RpError" { precondition(reply.payload[6] == 7) }
    switch req.a {
    case "other": return ("Other", reply.payload)
    case "none": return (nil, reply.payload)
    default: return (reply.variant, reply.payload)
    }
}

struct Carrier: CsilTransport {
    func call(service: String, op: String, request: [UInt8]) throws -> [UInt8] {
        fatalError("a multi-arm operation must use callReply")
    }

    func callReply(service: String, op: String, request: [UInt8]) throws -> (variant: String?, payload: [UInt8]) {
        try answer(service: service, op: op, request: request)
    }
}

struct AsyncCarrier: AsyncCsilTransport {
    func call(service: String, op: String, request: [UInt8]) async throws -> [UInt8] {
        fatalError("a multi-arm operation must use callReply")
    }

    func callReply(service: String, op: String, request: [UInt8]) async throws -> (variant: String?, payload: [UInt8]) {
        try answer(service: service, op: op, request: request)
    }
}

func check(_ name: String, _ call: (Req) async throws -> Resp) async throws {
    let ok = try await call(Req(a: "ok"))
    precondition(ok.b == "ok", "\(name): wrong success value")
    do {
        _ = try await call(Req(a: "limit"))
        fatalError("\(name): missing refusal")
    } catch let refusal as RpErrorRefusal {
        precondition(refusal.value.code == .applicationUserLimit)
    }
    for forced in ["other", "none"] {
        do {
            _ = try await call(Req(a: forced))
            fatalError("\(name): accepted variant \(forced)")
        } catch CsilCborError.typeMismatch {}
    }
    do {
        _ = try await call(Req(a: "unknown"))
        fatalError("\(name): missing transport error")
    } catch let error as CsilClientError {
        precondition(error.code == 2)
    }
}

let client = RpClient(transport: Carrier())
let asyncClient = RpAsyncClient(transport: AsyncCarrier())
try await check("sync") { try client.decryptToken($0) }
try await check("async") { try await asyncClient.decryptToken($0) }
print("swift: typed replies pass")
