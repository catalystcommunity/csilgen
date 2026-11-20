package community.catalyst.csilgen.generated

fun main() {
    check(RpErrorCode.AccountRateLimited.wireNumber == 42L)
    check(RpErrorCode.fromNumber(42L) == RpErrorCode.AccountRateLimited)
    check(RpErrorCode.fromNumber(1L) == null)
    val handler = object : Rp {
        override fun decryptToken(request: Req): Resp {
            if (request.a == "limit") throw RpErrorRefusal(RpError(RpErrorCode.ApplicationUserLimit, "limit"))
            return Resp("ok")
        }
    }
    val client = RpClient(object : Transport {
        override fun call(service: String, op: String, request: ByteArray): ByteArray {
            val req = decode<Req>(request)
            if (req.a == "unknown") throw ClientError(2, "unknown")
            val reply = encodeRpDecryptTokenReply { handler.decryptToken(req) }
            if (reply.variant == "RpError") check(reply.payload[6].toInt() == 7)
            return resolveRpcReply(service, op, reply.variant, reply.payload)
        }
    })
    check(client.decryptToken(Req("ok")).b == "ok")
    try {
        client.decryptToken(Req("limit"))
        error("missing refusal")
    } catch (e: RpErrorRefusal) {
        check(e.value.code == RpErrorCode.ApplicationUserLimit)
    }
    try {
        client.decryptToken(Req("unknown"))
        error("missing transport error")
    } catch (e: ClientError) {
        check(e.code == 2L)
    }
}
