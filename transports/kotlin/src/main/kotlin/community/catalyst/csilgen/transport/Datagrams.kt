// CSIL-Datagrams transport — unreliable, unordered, message-oriented — see
// csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header profiles. A
// datagram channel is single-service: the service is bound at channel setup, so datagrams
// carry no service ordinal.
//
// Per the spec the compact fixed-header profile uses narrow integer widths (op_ord=u8,
// seq=u16, epoch=u8); the cbor-array profile uses full CBOR uints. Those widths come from
// the docs, not from the conformance JSON, which does not type its numbers.
package community.catalyst.csilgen.transport

/** Conservative max datagram size (envelope + payload) safe across UDP/WebRTC/QUIC. */
const val MAX_DATAGRAM_DEFAULT: Int = 1200

/** A datagram in the CBOR-array (default) profile: [v, op_ord, seq, payload]. */
class Datagram(
    val opOrd: ULong,
    // The per-channel sequence; 0 means "unsequenced".
    val seq: ULong,
    // The opaque CBOR(message type) bytes.
    val payload: ByteArray,
) {
    fun encode(): ByteArray {
        val arr = CArray(listOf(CUInt(VERSION), CUInt(opOrd), CUInt(seq), tag24(payload)))
        return encodeValue(arr)
    }

    override fun equals(other: Any?): Boolean =
        other is Datagram && opOrd == other.opOrd && seq == other.seq &&
            payload.contentEquals(other.payload)

    override fun hashCode(): Int {
        var h = opOrd.hashCode()
        h = 31 * h + seq.hashCode()
        h = 31 * h + payload.contentHashCode()
        return h
    }

    override fun toString(): String = "Datagram(opOrd=$opOrd, seq=$seq, payload=${payload.size}B)"

    companion object {
        fun decode(b: ByteArray): Datagram {
            val v = decodeEnvelope(b)
            if (v !is CArray) throw malformed("datagram is not an array")
            if (v.items.size != 4) {
                throw malformed("datagram array has ${v.items.size} elements, expected 4")
            }
            val ver = asU64(v.items[0]) ?: throw malformed("datagram field not an integer")
            checkVersion(ver)
            val opOrd = asU64(v.items[1]) ?: throw malformed("datagram field not an integer")
            val seq = asU64(v.items[2]) ?: throw malformed("datagram field not an integer")
            return Datagram(opOrd = opOrd, seq = seq, payload = untag24(v.items[3]))
        }
    }
}

private const val COMPACT_VER: Int = 1
private const val FLAG_EPOCH: Int = 0b0001

/** A datagram in the compact fixed-header profile. Header layout:
 *  [ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8]) then the opaque body. The narrow widths
 *  are spec-mandated; values are kept in `UByte`/`UShort` so the unsigned ranges are exact. */
class CompactDatagram(
    val opOrd: UByte,
    val seq: UShort,
    // Present when the sender tracks restarts (sets the flags epoch bit).
    val epoch: UByte? = null,
    // The opaque body bytes (tag-24 CBOR or a raw media frame, by channel agreement).
    val body: ByteArray,
) {
    /** Set the epoch byte (and the flags bit that signals its presence). */
    fun withEpoch(epoch: UByte): CompactDatagram = CompactDatagram(opOrd, seq, epoch, body)

    fun encode(): ByteArray {
        val flags = if (epoch != null) FLAG_EPOCH else 0
        val seqInt = seq.toInt()
        val head = mutableListOf<Byte>(
            ((COMPACT_VER shl 4) or (flags and 0x0f)).toByte(),
            opOrd.toByte(),
            ((seqInt ushr 8) and 0xFF).toByte(),
            (seqInt and 0xFF).toByte(),
        )
        if (epoch != null) head.add(epoch.toByte())
        return head.toByteArray() + body
    }

    override fun equals(other: Any?): Boolean =
        other is CompactDatagram && opOrd == other.opOrd && seq == other.seq &&
            epoch == other.epoch && body.contentEquals(other.body)

    override fun hashCode(): Int {
        var h = opOrd.hashCode()
        h = 31 * h + seq.hashCode()
        h = 31 * h + epoch.hashCode()
        h = 31 * h + body.contentHashCode()
        return h
    }

    override fun toString(): String =
        "CompactDatagram(opOrd=$opOrd, seq=$seq, epoch=$epoch, body=${body.size}B)"

    companion object {
        fun decode(b: ByteArray): CompactDatagram {
            if (b.size < 4) throw malformed("compact datagram shorter than the 4-byte header")
            val b0 = b[0].toInt() and 0xFF
            val ver = b0 shr 4
            if (ver != COMPACT_VER) throw UnsupportedVersionException(ver.toULong())
            val flags = b0 and 0x0f
            val opOrd = (b[1].toInt() and 0xFF).toUByte()
            val seq = (((b[2].toInt() and 0xFF) shl 8) or (b[3].toInt() and 0xFF)).toUShort()
            var epoch: UByte? = null
            var bodyStart = 4
            if (flags and FLAG_EPOCH != 0) {
                if (b.size < 5) throw malformed("compact datagram flags claim an epoch byte that is absent")
                epoch = (b[4].toInt() and 0xFF).toUByte()
                bodyStart = 5
            }
            return CompactDatagram(
                opOrd = opOrd,
                seq = seq,
                epoch = epoch,
                body = b.copyOfRange(bodyStart, b.size),
            )
        }
    }
}

/** Classifies an incoming sequence number relative to what was last seen, for
 *  loss/reorder/restart detection. The transport detects; the app decides. */
enum class SeqEventKind {
    // The first datagram seen on the channel.
    FIRST,

    // Strictly newer than the last (possibly skipping some — a gap/loss).
    ADVANCED,

    // Not newer (a late or duplicate datagram).
    LATE_OR_DUPLICATE,

    // The sender restarted (epoch changed); seq numbering reset.
    RESTART,
}

/** A sequence classification plus the gap count for [SeqEventKind.ADVANCED]. */
data class SeqEvent(val kind: SeqEventKind, val gap: ULong = 0uL)

/** Tracks the last sequence/epoch per channel to classify arrivals. Unsequenced datagrams
 *  (seq 0) are reported as ADVANCED with gap 0. */
class SeqTracker {
    private var lastSeq: ULong? = null
    private var lastEpoch: UByte? = null

    /** Classify an arriving (seq, epoch) and update the tracker state. A restart fires only
     *  when a prior epoch existed and changed; no-epoch → first epoch is not a restart. */
    fun observe(seq: ULong, epoch: UByte?): SeqEvent {
        if (epoch != lastEpoch && lastEpoch != null) {
            lastEpoch = epoch
            lastSeq = seq
            return SeqEvent(SeqEventKind.RESTART)
        }
        lastEpoch = epoch
        // seq 0 marks an unsequenced datagram: it carries no ordering information, so it is
        // never late or duplicate. Report a zero-gap advance and leave the running sequence
        // untouched so a mix of sequenced and unsequenced still tracks the sequenced ones.
        if (seq == 0uL) return SeqEvent(SeqEventKind.ADVANCED, 0uL)
        val last = lastSeq
        if (last == null) {
            lastSeq = seq
            return SeqEvent(SeqEventKind.FIRST)
        }
        if (seq > last) {
            val gap = seq - last - 1uL
            lastSeq = seq
            return SeqEvent(SeqEventKind.ADVANCED, gap)
        }
        return SeqEvent(SeqEventKind.LATE_OR_DUPLICATE)
    }
}
