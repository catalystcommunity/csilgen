// CSIL-RPC transport — request/response/push envelopes — see csil-rpc-transport.md.
//
// Envelopes are `data class`es with a hand-written equals/hashCode wherever they hold a
// `ByteArray`: a data class's generated structural equality uses array *identity* for a
// `ByteArray` field, so two envelopes with equal payload bytes would compare unequal and
// the conformance decode-check would silently fail. The override uses `contentEquals`.
package community.catalyst.csilgen.transport

/** A CSIL-RPC request (client → server). `id`/`auth` are omitted from the canonical map
 *  when null. */
class RpcRequest(
    val service: String,
    val op: String,
    // The opaque CBOR(request type) bytes (wrapped in tag 24 on the wire).
    val payload: ByteArray,
    val id: ULong? = null,
    val auth: String? = null,
) {
    /** Set the correlation id (required on multiplexed carriers). */
    fun withId(id: ULong): RpcRequest = RpcRequest(service, op, payload, id, auth)

    /** Set the per-request credential for caller-scoped operations. */
    fun withAuth(auth: String): RpcRequest = RpcRequest(service, op, payload, id, auth)

    fun encode(): ByteArray {
        val entries = mutableListOf(
            CEntry(CText("v"), CUInt(VERSION)),
            CEntry(CText("service"), CText(service)),
            CEntry(CText("op"), CText(op)),
            CEntry(CText("payload"), tag24(payload)),
        )
        if (id != null) entries.add(CEntry(CText("id"), CUInt(id)))
        if (auth != null) entries.add(CEntry(CText("auth"), CText(auth)))
        return encodeValue(canonMap(entries))
    }

    override fun equals(other: Any?): Boolean =
        other is RpcRequest && service == other.service && op == other.op &&
            payload.contentEquals(other.payload) && id == other.id && auth == other.auth

    override fun hashCode(): Int {
        var h = service.hashCode()
        h = 31 * h + op.hashCode()
        h = 31 * h + payload.contentHashCode()
        h = 31 * h + id.hashCode()
        h = 31 * h + auth.hashCode()
        return h
    }

    override fun toString(): String =
        "RpcRequest(service=$service, op=$op, id=$id, auth=$auth, payload=${payload.size}B)"

    companion object {
        /** Build a request with no correlation id and no per-request auth. */
        fun of(service: String, op: String, payload: ByteArray): RpcRequest =
            RpcRequest(service, op, payload)

        fun decode(b: ByteArray): RpcRequest {
            val v = decodeEnvelope(b)
            checkVersion(getUint(v, "v"))
            val p = mapGet(v, "payload") ?: throw malformed("missing 'payload'")
            val payload = untag24(p)
            return RpcRequest(
                service = getText(v, "service"),
                op = getText(v, "op"),
                payload = payload,
                id = getUintOpt(v, "id"),
                auth = getTextOpt(v, "auth"),
            )
        }
    }
}

/** A CSIL-RPC response (server → client). `payload` is present (a tag-24 byte string) even
 *  when empty on a non-zero status. */
class RpcResponse(
    val status: Status,
    // The opaque CBOR(output type) bytes; empty when status is non-zero.
    val payload: ByteArray,
    val id: ULong? = null,
    // Names which declared output-choice arm `payload` decodes to (the CSIL type name).
    val variant: String? = null,
    val error: String? = null,
) {
    /** Set the echoed correlation id. */
    fun withId(id: ULong?): RpcResponse = RpcResponse(status, payload, id, variant, error)

    fun encode(): ByteArray {
        val entries = mutableListOf(
            CEntry(CText("v"), CUInt(VERSION)),
            CEntry(CText("status"), CInt(status.code.toLong())),
            CEntry(CText("payload"), tag24(payload)),
        )
        if (id != null) entries.add(CEntry(CText("id"), CUInt(id)))
        if (variant != null) entries.add(CEntry(CText("variant"), CText(variant)))
        if (error != null) entries.add(CEntry(CText("error"), CText(error)))
        return encodeValue(canonMap(entries))
    }

    /** Return a StatusException for a non-ok response, or null when the response carries a
     *  typed reply (status 0). Callers use this after decode to surface transport failures
     *  distinctly from application errors. */
    fun asTransportError(): StatusException? {
        if (status.isOk) return null
        return StatusException(status.statusName, status.code, error ?: "")
    }

    override fun equals(other: Any?): Boolean =
        other is RpcResponse && status == other.status && payload.contentEquals(other.payload) &&
            id == other.id && variant == other.variant && error == other.error

    override fun hashCode(): Int {
        var h = status.hashCode()
        h = 31 * h + payload.contentHashCode()
        h = 31 * h + id.hashCode()
        h = 31 * h + variant.hashCode()
        h = 31 * h + error.hashCode()
        return h
    }

    override fun toString(): String =
        "RpcResponse(status=$status, id=$id, variant=$variant, error=$error, payload=${payload.size}B)"

    companion object {
        /** Build a successful (status ok) typed reply. */
        fun ok(variant: String, payload: ByteArray): RpcResponse =
            RpcResponse(Status.OK, payload, variant = variant)

        /** Build a transport-level failure (no typed payload). */
        fun transportError(status: Status, message: String): RpcResponse =
            RpcResponse(status, ByteArray(0), error = message)

        fun decode(b: ByteArray): RpcResponse {
            val v = decodeEnvelope(b)
            checkVersion(getUint(v, "v"))
            // payload is present but may be an empty byte string on error.
            var payload = ByteArray(0)
            val p = mapGet(v, "payload")
            if (p != null) payload = untag24(p)
            return RpcResponse(
                status = Status.fromCode(getInt(v, "status").toInt()),
                payload = payload,
                id = getUintOpt(v, "id"),
                variant = getTextOpt(v, "variant"),
                error = getTextOpt(v, "error"),
            )
        }
    }
}

/** A CSIL-RPC server push (server → client) for `<-` operations. */
class RpcPush(
    val service: String,
    val event: String,
    val payload: ByteArray,
) {
    fun encode(): ByteArray {
        val entries = listOf(
            CEntry(CText("v"), CUInt(VERSION)),
            CEntry(CText("service"), CText(service)),
            CEntry(CText("event"), CText(event)),
            CEntry(CText("payload"), tag24(payload)),
        )
        return encodeValue(canonMap(entries))
    }

    override fun equals(other: Any?): Boolean =
        other is RpcPush && service == other.service && event == other.event &&
            payload.contentEquals(other.payload)

    override fun hashCode(): Int {
        var h = service.hashCode()
        h = 31 * h + event.hashCode()
        h = 31 * h + payload.contentHashCode()
        return h
    }

    override fun toString(): String = "RpcPush(service=$service, event=$event, payload=${payload.size}B)"

    companion object {
        fun decode(b: ByteArray): RpcPush {
            val v = decodeEnvelope(b)
            checkVersion(getUint(v, "v"))
            val p = mapGet(v, "payload") ?: throw malformed("missing 'payload'")
            return RpcPush(
                service = getText(v, "service"),
                event = getText(v, "event"),
                payload = untag24(p),
            )
        }
    }
}

/** What a server handler returns for one request: a typed reply on success, or a transport
 *  status on failure. A sealed interface gives the host an exhaustive `when`. */
sealed interface HandlerOutcome

/** A successful typed reply (variant name + encoded payload). */
class Reply(val variant: String, val payload: ByteArray) : HandlerOutcome {
    override fun equals(other: Any?): Boolean =
        other is Reply && variant == other.variant && payload.contentEquals(other.payload)

    override fun hashCode(): Int = 31 * variant.hashCode() + payload.contentHashCode()
}

/** A transport-status outcome (no typed payload). */
data class TransportOutcome(val status: Status, val message: String) : HandlerOutcome

/** A CSIL-RPC client over a frame carrier. The carrier is injected (bring your own); the
 *  client owns the envelope and a per-connection monotonic correlation id. */
class RpcClient(
    val carrier: FrameCarrier,
    // true assigns a correlation id to every request (required on WS / pipelined streams);
    // false omits it (one-in-flight carriers such as HTTP).
    private val multiplexed: Boolean,
) {
    private var nextId: ULong = 1uL

    /** Invoke service/op with an encoded request payload, returning the decoded response. A
     *  non-zero transport status is surfaced as a StatusException. */
    fun call(service: String, op: String, payload: ByteArray, auth: String? = null): RpcResponse {
        var req = RpcRequest(service, op, payload, auth = auth)
        if (multiplexed) {
            req = req.withId(nextId)
            nextId++
        }
        carrier.sendFrame(req.encode())
        val inFrame = carrier.recvFrame() ?: throw CarrierException("connection closed before response")
        val resp = RpcResponse.decode(inFrame)
        resp.asTransportError()?.let { throw it }
        return resp
    }
}

/** A CSIL-RPC server over a frame carrier. The host supplies a handler mapping a decoded
 *  request to an outcome; the generated router is the natural implementation of that
 *  handler. Synchronous and single-threaded by design — the host owns any threading. */
class RpcServer(private val carrier: FrameCarrier) {
    /** Read one request, dispatch it through `handler`, and write the response. Returns
     *  false at a clean end of stream. */
    fun serveOne(handler: (RpcRequest) -> HandlerOutcome): Boolean {
        val frame = carrier.recvFrame() ?: return false
        val resp = try {
            val req = RpcRequest.decode(frame)
            when (val outcome = handler(req)) {
                is Reply -> RpcResponse.ok(outcome.variant, outcome.payload).withId(req.id)
                is TransportOutcome -> RpcResponse.transportError(outcome.status, outcome.message).withId(req.id)
            }
        } catch (e: TransportException) {
            RpcResponse.transportError(Status.MALFORMED_ENVELOPE, e.message ?: "malformed envelope")
        }
        carrier.sendFrame(resp.encode())
        return true
    }
}
