// Conventions shared by every CSIL transport — see csil-transport-conventions.md.
//
// This file owns the parts the three transports agree on: the version constant, the
// transport status registry, tag-24 payload wrap/unwrap, the max-frame guard, the
// exception hierarchy, and the canonical-CBOR field accessors the envelopes build on
// so their bytes match the conformance vectors regardless of class layout.
package community.catalyst.csilgen.transport

/** The current transport version. A new value is minted only for a breaking change to
 *  envelope layout or semantics. */
const val VERSION: ULong = 1uL

/** The CBOR semantic tag wrapping an embedded, opaque CBOR data item (RFC 8949 §3.4.5.1). */
const val TAG_ENCODED_CBOR: ULong = 24uL

/** The reserved service ordinal for the transport control plane (Events lifecycle). */
const val CONTROL_SERVICE_ORD: ULong = 0uL

/** Default max encoded envelope size for stream/message carriers (16 MiB). A carrier
 *  rejects anything larger before allocating for it. */
const val MAX_FRAME_DEFAULT: Int = 16 * 1024 * 1024

/** Base of the transport exception hierarchy. Kotlin has no checked exceptions, so the
 *  references raise rather than thread a `Result` through every call. */
open class TransportException(message: String, cause: Throwable? = null) :
    RuntimeException(message, cause)

/** A frame exceeded the max-frame guard; the receiver rejects it before allocating. */
class FrameTooLargeException(val got: Long, val max: Int) :
    TransportException("frame of $got bytes exceeds max-frame guard of $max bytes")

/** An envelope's version is not supported. */
class UnsupportedVersionException(val v: ULong) :
    TransportException("unsupported transport version $v")

/** A carrier-level transport failure (a wrapped I/O failure or a carrier protocol
 *  violation). */
class CarrierException(message: String, cause: Throwable? = null) :
    TransportException(message, cause)

/** A non-zero transport status returned by a peer, distinct from a host's application
 *  errors (which ride inside the payload as a declared `/ ErrorType` arm). */
class StatusException(val status: String, val code: Int, val detail: String) :
    TransportException(
        if (detail.isNotEmpty()) "transport status $status ($code): $detail"
        else "transport status $status ($code)",
    )

/** A transport-level status. Modeled as a value class over the wire `Int` (not an enum)
 *  because the registry allows unknown/host codes (>= 64) an enum could not represent;
 *  unknown codes round-trip verbatim. */
@JvmInline
value class Status(val code: Int) {
    /** Whether the status indicates a typed reply is present. */
    val isOk: Boolean get() = code == 0

    /** The registry name for the status, or "other" for codes outside it. */
    val statusName: String
        get() = when (code) {
            0 -> "ok"
            1 -> "malformed-envelope"
            2 -> "unknown-service-or-op"
            3 -> "unauthenticated"
            4 -> "forbidden"
            5 -> "version-unsupported"
            6 -> "internal"
            7 -> "unavailable"
            8 -> "deadline-exceeded"
            else -> "other"
        }

    companion object {
        val OK = Status(0)
        val MALFORMED_ENVELOPE = Status(1)
        val UNKNOWN_SERVICE_OR_OP = Status(2)
        val UNAUTHENTICATED = Status(3)
        val FORBIDDEN = Status(4)
        val VERSION_UNSUPPORTED = Status(5)
        val INTERNAL = Status(6)
        val UNAVAILABLE = Status(7)
        val DEADLINE_EXCEEDED = Status(8)

        /** Map a wire code onto a Status, preserving host-defined and unknown codes. */
        fun fromCode(code: Int): Status = Status(code)
    }
}

/** Build a malformed-envelope exception with a consistent prefix. */
internal fun malformed(message: String): CborException = CborException("malformed envelope: $message")

/** Wrap opaque payload bytes (themselves a CBOR item) in tag 24. The bytes are copied
 *  so a later mutation of the caller's array cannot change the envelope. */
internal fun tag24(payload: ByteArray): CborValue = CTag(TAG_ENCODED_CBOR, CBytes(payload.copyOf()))

/** Extract the opaque payload bytes from a tag-24 value. */
internal fun untag24(v: CborValue): ByteArray {
    if (v !is CTag || v.num != TAG_ENCODED_CBOR) throw malformed("expected a tag-24 (encoded-cbor) payload")
    val content = v.content
    if (content !is CBytes) throw malformed("tag-24 payload is not a byte string")
    return content.b.copyOf()
}

/** Look up a text key in a decoded CBOR map value. */
internal fun mapGet(v: CborValue, key: String): CborValue? {
    if (v !is CMap) return null
    for (e in v.entries) {
        val k = e.key
        if (k is CText && k.s == key) return e.value
    }
    return null
}

/** Read a non-negative integer from a decoded CBOR integer value, or null. */
internal fun asU64(v: CborValue): ULong? = when (v) {
    is CUInt -> v.v
    is CInt -> if (v.v >= 0) v.v.toULong() else null
    else -> null
}

/** Read a signed integer from a decoded CBOR integer value, or null. */
internal fun asI64(v: CborValue): Long? = when (v) {
    is CUInt -> v.v.toLong()
    is CInt -> v.v
    else -> null
}

internal fun getUint(m: CborValue, key: String): ULong {
    val v = mapGet(m, key) ?: throw malformed("missing or non-integer field '$key'")
    return asU64(v) ?: throw malformed("field '$key' is not a non-negative integer")
}

internal fun getInt(m: CborValue, key: String): Long {
    val v = mapGet(m, key) ?: throw malformed("missing or non-integer field '$key'")
    return asI64(v) ?: throw malformed("missing or non-integer field '$key'")
}

internal fun getText(m: CborValue, key: String): String {
    val v = mapGet(m, key) ?: throw malformed("missing or non-text field '$key'")
    if (v !is CText) throw malformed("missing or non-text field '$key'")
    return v.s
}

internal fun getTextOpt(m: CborValue, key: String): String? {
    val v = mapGet(m, key) ?: return null
    return if (v is CText) v.s else null
}

internal fun getUintOpt(m: CborValue, key: String): ULong? {
    val v = mapGet(m, key) ?: return null
    return asU64(v)
}

/** Verify a decoded envelope's version field, throwing so an unknown version is never
 *  silently misparsed. */
internal fun checkVersion(v: ULong) {
    if (v != VERSION) throw UnsupportedVersionException(v)
}
