// CSIL-Events transport — typed bidirectional event streams — see
// csil-events-transport.md. Verbose (text-keyed) and compact (positional) profiles, plus
// the control plane (service ordinal 0) lifecycle events.
package community.catalyst.csilgen.transport

/** Which wire profile a connection uses for its lifetime, fixed by hello. */
enum class Profile {
    VERBOSE,
    COMPACT,
    ;

    /** The wire name offered/selected during negotiation. */
    val wireName: String get() = when (this) {
        VERBOSE -> "verbose"
        COMPACT -> "compact"
    }

    companion object {
        /** Map a wire profile name onto a Profile, or null if unknown. */
        fun parse(s: String): Profile? = when (s) {
            "verbose" -> VERBOSE
            "compact" -> COMPACT
            else -> null
        }
    }
}

/** One typed event flowing in either direction. Identified by service+operation (verbose)
 *  or by their ordinals (compact), and carrying an optional correlation id. */
class Event(
    val payload: ByteArray,
    // The CSIL service name (verbose). null on a single-service verbose connection.
    val service: String? = null,
    // The service ordinal (compact). Always present in compact frames.
    val serviceOrd: ULong? = null,
    // The CSIL operation name (verbose).
    val event: String? = null,
    // The operation ordinal (compact).
    val opOrd: ULong? = null,
    val id: ULong? = null,
) {
    /** Set the correlation id. */
    fun withId(id: ULong): Event =
        Event(payload, service, serviceOrd, event, opOrd, id)

    /** Serialize the event under the given profile. */
    fun encode(profile: Profile): ByteArray = when (profile) {
        Profile.VERBOSE -> encodeVerbose()
        Profile.COMPACT -> encodeCompact()
    }

    private fun encodeVerbose(): ByteArray {
        val name = event ?: throw malformed("verbose event missing 'event' name")
        val entries = mutableListOf(
            CEntry(CText("event"), CText(name)),
            CEntry(CText("payload"), tag24(payload)),
        )
        if (service != null) entries.add(CEntry(CText("service"), CText(service)))
        if (id != null) entries.add(CEntry(CText("id"), CUInt(id)))
        return encodeValue(canonMap(entries))
    }

    private fun encodeCompact(): ByteArray {
        val svc = serviceOrd ?: throw malformed("compact event missing service ordinal")
        val op = opOrd ?: throw malformed("compact event missing op ordinal")
        val items = mutableListOf<CborValue>(CUInt(svc), CUInt(op))
        if (id != null) items.add(CUInt(id))
        items.add(tag24(payload))
        return encodeValue(CArray(items))
    }

    override fun equals(other: Any?): Boolean =
        other is Event && payload.contentEquals(other.payload) && service == other.service &&
            serviceOrd == other.serviceOrd && event == other.event && opOrd == other.opOrd &&
            id == other.id

    override fun hashCode(): Int {
        var h = payload.contentHashCode()
        h = 31 * h + service.hashCode()
        h = 31 * h + serviceOrd.hashCode()
        h = 31 * h + event.hashCode()
        h = 31 * h + opOrd.hashCode()
        h = 31 * h + id.hashCode()
        return h
    }

    override fun toString(): String =
        "Event(service=$service, serviceOrd=$serviceOrd, event=$event, opOrd=$opOrd, id=$id, payload=${payload.size}B)"

    companion object {
        /** Build a verbose event by name. */
        fun verbose(service: String?, event: String, payload: ByteArray): Event =
            Event(payload = payload, service = service, event = event)

        /** Build a compact event by ordinals. */
        fun compact(serviceOrd: ULong, opOrd: ULong, payload: ByteArray): Event =
            Event(payload = payload, serviceOrd = serviceOrd, opOrd = opOrd)

        /** Parse an event under the given profile. */
        fun decode(b: ByteArray, profile: Profile): Event = when (profile) {
            Profile.VERBOSE -> decodeVerbose(b)
            Profile.COMPACT -> decodeCompact(b)
        }

        private fun decodeVerbose(b: ByteArray): Event {
            val v = decodeEnvelope(b)
            val p = mapGet(v, "payload") ?: throw malformed("missing 'payload'")
            return Event(
                payload = untag24(p),
                service = getTextOpt(v, "service"),
                event = getText(v, "event"),
                id = getUintOpt(v, "id"),
            )
        }

        private fun decodeCompact(b: ByteArray): Event {
            val v = decodeEnvelope(b)
            if (v !is CArray) throw malformed("compact event is not an array")
            // 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
            val items = v.items
            val serviceOrdV: CborValue
            val opOrdV: CborValue
            val payloadV: CborValue
            var idV: CborValue? = null
            when (items.size) {
                3 -> { serviceOrdV = items[0]; opOrdV = items[1]; payloadV = items[2] }
                4 -> { serviceOrdV = items[0]; opOrdV = items[1]; idV = items[2]; payloadV = items[3] }
                else -> throw malformed("compact event array has ${items.size} elements, expected 3 or 4")
            }
            val serviceOrd = asU64(serviceOrdV) ?: throw malformed("ordinal is not an integer")
            val opOrd = asU64(opOrdV) ?: throw malformed("ordinal is not an integer")
            val payload = untag24(payloadV)
            val id = if (idV != null) (asU64(idV) ?: throw malformed("ordinal is not an integer")) else null
            return Event(payload = payload, serviceOrd = serviceOrd, opOrd = opOrd, id = id)
        }
    }
}

/** Control-plane operation ordinals (under service ordinal 0). */
object Control {
    const val HELLO: ULong = 0uL
    const val HELLO_ACK: ULong = 1uL
    const val PING: ULong = 2uL
    const val PONG: ULong = 3uL
    const val CLOSE: ULong = 4uL
    const val ERROR: ULong = 5uL

    // Verbose control-event names (the `$`-sigil names).
    const val HELLO_NAME = "\$hello"
    const val HELLO_ACK_NAME = "\$hello-ack"
    const val PING_NAME = "\$ping"
    const val PONG_NAME = "\$pong"
    const val CLOSE_NAME = "\$close"
    const val ERROR_NAME = "\$error"
}

/** The `$hello` payload offered by the connection initiator. */
class Hello(
    val versions: List<ULong>,
    val profiles: List<String>,
    val service: String? = null,
    val auth: String? = null,
) {
    fun encode(): ByteArray {
        val entries = mutableListOf(
            CEntry(CText("versions"), CArray(versions.map { CUInt(it) })),
            CEntry(CText("profiles"), CArray(profiles.map { CText(it) })),
        )
        if (service != null) entries.add(CEntry(CText("service"), CText(service)))
        if (auth != null) entries.add(CEntry(CText("auth"), CText(auth)))
        return encodeValue(canonMap(entries))
    }

    /** Select a profile from this hello's offers, honoring the peer's preference order and
     *  what the receiver supports. Returns the chosen (version, profile), or null if nothing
     *  is mutually supported. */
    fun negotiate(supported: List<Profile>): Pair<ULong, Profile>? {
        if (versions.none { it == VERSION }) return null
        for (offered in profiles) {
            val p = Profile.parse(offered) ?: continue
            if (p in supported) return Pair(VERSION, p)
        }
        return null
    }

    companion object {
        fun decode(b: ByteArray): Hello {
            val v = decodeEnvelope(b)
            val versRaw = mapGet(v, "versions")
            if (versRaw !is CArray) throw malformed("hello missing 'versions'")
            val versions = versRaw.items.mapNotNull { asU64(it) }
            val profRaw = mapGet(v, "profiles")
            if (profRaw !is CArray) throw malformed("hello missing 'profiles'")
            val profiles = profRaw.items.mapNotNull { (it as? CText)?.s }
            return Hello(
                versions = versions,
                profiles = profiles,
                service = getTextOpt(v, "service"),
                auth = getTextOpt(v, "auth"),
            )
        }
    }
}

/** The `$hello-ack` payload returned by the peer. */
data class HelloAck(
    val v: ULong,
    val profile: String,
    val session: String? = null,
) {
    fun encode(): ByteArray {
        val entries = mutableListOf(
            CEntry(CText("v"), CUInt(v)),
            CEntry(CText("profile"), CText(profile)),
        )
        if (session != null) entries.add(CEntry(CText("session"), CText(session)))
        return encodeValue(canonMap(entries))
    }

    companion object {
        fun decode(b: ByteArray): HelloAck {
            val cv = decodeEnvelope(b)
            return HelloAck(
                v = getUint(cv, "v"),
                profile = getText(cv, "profile"),
                session = getTextOpt(cv, "session"),
            )
        }
    }
}

/** A `$ping`/`$pong` heartbeat payload. */
data class Heartbeat(
    val nonce: ULong,
    val at: ULong? = null,
) {
    fun encode(): ByteArray {
        val entries = mutableListOf(CEntry(CText("nonce"), CUInt(nonce)))
        if (at != null) entries.add(CEntry(CText("at"), CUInt(at)))
        return encodeValue(canonMap(entries))
    }

    companion object {
        fun decode(b: ByteArray): Heartbeat {
            val v = decodeEnvelope(b)
            return Heartbeat(nonce = getUint(v, "nonce"), at = getUintOpt(v, "at"))
        }
    }
}

/** A `$close` payload. */
data class Close(
    val status: Status,
    val reason: String? = null,
) {
    fun encode(): ByteArray {
        val entries = mutableListOf(CEntry(CText("status"), CInt(status.code.toLong())))
        if (reason != null) entries.add(CEntry(CText("reason"), CText(reason)))
        return encodeValue(canonMap(entries))
    }

    companion object {
        fun decode(b: ByteArray): Close {
            val v = decodeEnvelope(b)
            return Close(status = Status.fromCode(getInt(v, "status").toInt()), reason = getTextOpt(v, "reason"))
        }
    }
}
