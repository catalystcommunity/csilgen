package community.catalyst.csilgen.generated

val handler = object : Rp {
    override fun decryptToken(request: Req): Resp {
        if (request.a == "limit") throw RpErrorRefusal(RpError(RpErrorCode.ApplicationUserLimit, "limit"))
        return Resp("ok")
    }
}

// A fake carrier: it answers with the variant the generated server encoder picks, unless
// a variant is forced; the request "none" gets a reply with no variant.
fun client(forced: String? = null) = RpClient(object : Transport {
    override fun call(service: String, op: String, request: ByteArray): ByteArray =
        error("a multi-arm op must use callReply")

    override fun callReply(service: String, op: String, request: ByteArray): WireReply {
        check(service == "Rp" && op == "decrypt-token")
        val req = decode<Req>(request)
        if (req.a == "unknown") throw ClientError(2, "unknown")
        val reply = encodeRpDecryptTokenReply { handler.decryptToken(req) }
        if (reply.variant == "RpError") check(reply.payload[6].toInt() == 7)
        if (req.a == "none") return WireReply(null, reply.payload)
        return WireReply(forced ?: reply.variant, reply.payload)
    }
})

fun main() {
    check(RpErrorCode.AccountRateLimited.wireNumber == 42L)
    check(RpErrorCode.fromNumber(42L) == RpErrorCode.AccountRateLimited)
    check(RpErrorCode.fromNumber(1L) == null)
    val client = client()
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
    try {
        client.decryptToken(Req("none"))
        error("missing variant accepted")
    } catch (_: CborError) {
    }
    try {
        client("Other").decryptToken(Req("ok"))
        error("unknown variant accepted")
    } catch (_: CborError) {
    }
    println("kotlin typed replies: ok")
}
