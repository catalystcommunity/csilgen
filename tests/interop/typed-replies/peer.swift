struct Handler: Rp {
    func decryptToken(_ request: Req) throws -> Resp {
        if request.a == "limit" {
            throw RpErrorRefusal(value: RpError(code: .applicationUserLimit, message: "limit"))
        }

precondition(RpErrorCode.accountRateLimited.wireNumber == 42)
precondition(RpErrorCode(wireNumber: 42) == .accountRateLimited)
precondition(RpErrorCode(wireNumber: 1) == nil)
        return Resp(b: "ok")
    }
}

struct Carrier: CsilTransport {
    func call(service: String, op: String, request: [UInt8]) throws -> [UInt8] {
        let req = try Req.fromCbor(request)
        if req.a == "unknown" { throw CsilClientError(code: 2, message: "unknown") }
        let reply = try encodeRpDecryptTokenReply { try Handler().decryptToken(req) }
        if reply.variant == "RpError" { precondition(reply.payload[6] == 7) }
        return try resolveRpcReply(service: service, op: op, variant: reply.variant, payload: reply.payload)
    }
}

let client = RpClient(transport: Carrier())
let ok = try client.decryptToken(Req(a: "ok"))
precondition(ok.b == "ok")
do {
    _ = try client.decryptToken(Req(a: "limit"))
    fatalError("missing refusal")
} catch let refusal as RpErrorRefusal {
    precondition(refusal.value.code == .applicationUserLimit)
}
do {
    _ = try client.decryptToken(Req(a: "unknown"))
    fatalError("missing transport error")
} catch let error as CsilClientError {
    precondition(error.code == 2)
}
