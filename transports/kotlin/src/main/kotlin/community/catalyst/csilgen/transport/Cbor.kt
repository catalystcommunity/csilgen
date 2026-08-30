// Minimal canonical CBOR codec (RFC 8949). Hand-written and dependency-free so the
// transport library stays offline-testable and we never inherit a third-party CBOR
// library's (non-)determinism. It supports exactly what the CSIL envelopes need —
// unsigned ints, negative ints, text strings, byte strings, arrays, maps, and tag 24
// — and nothing else. Maps use core deterministic encoding: entries sorted by the
// bytewise lexicographic order of their encoded keys, matching the Go/Rust references
// so bytes are byte-identical to the conformance vectors.
//
// A JVM Byte is signed, but the wire is unsigned: every wire byte is read back as
// `b.toInt() and 0xFF`, head arguments use `ULong`, and the map-key comparator does an
// explicit unsigned compare. Plain `ByteArray` is used on the wire (the unsigned array
// types are still beta); only scalar `ULong`/`UByte` arithmetic uses the unsigned types.
package community.catalyst.csilgen.transport

import java.io.ByteArrayOutputStream

/** In-memory model of the CBOR items the envelopes use; layout is independent of any
 *  Kotlin class layout, so transports build envelopes from the canonical helpers. */
sealed interface CborValue

data class CUInt(val v: ULong) : CborValue

/** Signed integer; encodes negative values as CBOR major type 1. */
data class CInt(val v: Long) : CborValue

data class CText(val s: String) : CborValue

/** A byte string. `ByteArray` has identity equality, so content semantics are forced. */
class CBytes(val b: ByteArray) : CborValue {
    override fun equals(other: Any?): Boolean = other is CBytes && b.contentEquals(other.b)
    override fun hashCode(): Int = b.contentHashCode()
}

data class CArray(val items: List<CborValue>) : CborValue

data class CEntry(val key: CborValue, val value: CborValue)

data class CMap(val entries: List<CEntry>) : CborValue

data class CTag(val num: ULong, val content: CborValue) : CborValue

/** Thrown for any malformed/unsupported CBOR encountered while decoding. */
class CborException(message: String) : TransportException(message)

/** Emit the initial byte (major type in the high three bits) plus the shortest-form
 *  argument bytes for n, big-endian via explicit shifts, per deterministic encoding. */
private fun writeHead(out: ByteArrayOutputStream, major: Int, n: ULong) {
    val mt = major shl 5
    when {
        n < 24uL -> out.write(mt or n.toInt())
        n < 0x100uL -> {
            out.write(mt or 24)
            out.write((n and 0xFFuL).toInt())
        }
        n < 0x10000uL -> {
            out.write(mt or 25)
            out.write(((n shr 8) and 0xFFuL).toInt())
            out.write((n and 0xFFuL).toInt())
        }
        n < 0x100000000uL -> {
            out.write(mt or 26)
            for (s in intArrayOf(24, 16, 8, 0)) out.write(((n shr s) and 0xFFuL).toInt())
        }
        else -> {
            out.write(mt or 27)
            for (s in intArrayOf(56, 48, 40, 32, 24, 16, 8, 0)) out.write(((n shr s) and 0xFFuL).toInt())
        }
    }
}

private fun encodeInto(out: ByteArrayOutputStream, v: CborValue) {
    when (v) {
        is CUInt -> writeHead(out, 0, v.v)
        is CInt ->
            // CBOR negative ints encode -1-n; the argument is the magnitude minus one.
            if (v.v >= 0) writeHead(out, 0, v.v.toULong()) else writeHead(out, 1, (-1L - v.v).toULong())
        is CText -> {
            val bytes = v.s.toByteArray(Charsets.UTF_8)
            writeHead(out, 3, bytes.size.toULong())
            out.write(bytes)
        }
        is CBytes -> {
            writeHead(out, 2, v.b.size.toULong())
            out.write(v.b)
        }
        is CArray -> {
            writeHead(out, 4, v.items.size.toULong())
            v.items.forEach { encodeInto(out, it) }
        }
        is CMap -> {
            writeHead(out, 5, v.entries.size.toULong())
            v.entries.forEach {
                encodeInto(out, it.key)
                encodeInto(out, it.value)
            }
        }
        is CTag -> {
            writeHead(out, 6, v.num)
            encodeInto(out, v.content)
        }
    }
}

/** Serialize a value to canonical CBOR bytes. */
fun encodeValue(v: CborValue): ByteArray {
    val out = ByteArrayOutputStream()
    encodeInto(out, v)
    return out.toByteArray()
}

/** Unsigned bytewise lexicographic compare, shorter-first — a `Byte` is signed, so
 *  each byte is masked to its unsigned value before comparing. */
private fun compareBytes(a: ByteArray, b: ByteArray): Int {
    val n = minOf(a.size, b.size)
    for (i in 0 until n) {
        val ai = a[i].toInt() and 0xFF
        val bi = b[i].toInt() and 0xFF
        if (ai != bi) return ai - bi
    }
    return a.size - b.size
}

/** Build a deterministically-keyed CBOR map: entries sorted by the bytewise
 *  lexicographic order of their *encoded* keys (RFC 8949 §4.2.1). `sortedWith` is
 *  stable, matching the Go reference's stable sort. */
fun canonMap(entries: List<CEntry>): CMap {
    val sorted = entries.sortedWith { a, b -> compareBytes(encodeValue(a.key), encodeValue(b.key)) }
    return CMap(sorted)
}

/** Decode a complete envelope: one self-contained CBOR item with no trailing bytes
 *  (an envelope is a single CBOR item, so leftover bytes are a malformed frame). */
fun decodeEnvelope(b: ByteArray): CborValue {
    val (v, n) = decodeValue(b, 0, 0)
    if (n != b.size) throw CborException("trailing bytes after CBOR item")
    return v
}

/** Parse one CBOR item starting at `off`, returning it and the number of bytes
 *  consumed (relative to `off`). The recursive workhorse for nested items. */
private fun decodeValue(b: ByteArray, off: Int, depth: Int): Pair<CborValue, Int> {
    if (depth > 64) throw CborException("CBOR nesting limit exceeded")
    if (off >= b.size) throw CborException("empty input")
    val ib = b[off].toInt() and 0xFF
    val major = ib shr 5
    val low = ib and 0x1f
    val (arg, headLen) = readArg(b, off, low)
    when (major) {
        0 -> return Pair(CUInt(arg), headLen)
        1 -> {
            // An arg above Long.MAX_VALUE names a value below Long.MIN_VALUE, which
            // would silently wrap if forced into a Long.
            if (arg > Long.MAX_VALUE.toULong()) throw CborException("negative integer out of Long range")
            return Pair(CInt(-1L - arg.toLong()), headLen)
        }
        2 -> {
            val len = boundedLen(arg)
            val start = off + headLen
            if (b.size.toLong() < start.toLong() + len.toLong()) throw CborException("truncated byte string")
            return Pair(CBytes(b.copyOfRange(start, start + len)), headLen + len)
        }
        3 -> {
            val len = boundedLen(arg)
            val start = off + headLen
            if (b.size.toLong() < start.toLong() + len.toLong()) throw CborException("truncated text string")
            val text = try {
                Charsets.UTF_8.newDecoder()
                    .onMalformedInput(java.nio.charset.CodingErrorAction.REPORT)
                    .onUnmappableCharacter(java.nio.charset.CodingErrorAction.REPORT)
                    .decode(java.nio.ByteBuffer.wrap(b, start, len)).toString()
            } catch (_: java.nio.charset.CharacterCodingException) {
                throw CborException("invalid UTF-8 text string")
            }
            return Pair(CText(text), headLen + len)
        }
        4 -> {
            var offset = off + headLen
            if (arg > (b.size - offset).toULong()) throw CborException("array length exceeds remaining input")
            val items = ArrayList<CborValue>()
            var i = 0uL
            while (i < arg) {
                val (item, m) = decodeValue(b, offset, depth + 1)
                items.add(item)
                offset += m
                i++
            }
            return Pair(CArray(items), offset - off)
        }
        5 -> {
            var offset = off + headLen
            if (arg > (b.size - offset).toULong()) throw CborException("map length exceeds remaining input")
            val entries = ArrayList<CEntry>()
            var i = 0uL
            while (i < arg) {
                val (k, m1) = decodeValue(b, offset, depth + 1)
                offset += m1
                val (v, m2) = decodeValue(b, offset, depth + 1)
                offset += m2
                entries.add(CEntry(k, v))
                i++
            }
            return Pair(CMap(entries), offset - off)
        }
        6 -> {
            val (content, m) = decodeValue(b, off + headLen, depth + 1)
            return Pair(CTag(arg, content), headLen + m)
        }
        else -> throw CborException("unsupported major type $major")
    }
}

/** A length argument narrowed to Int, rejecting anything that cannot be a valid frame
 *  offset (the 16 MiB frame guard bounds real lengths well below this). */
private fun boundedLen(arg: ULong): Int {
    if (arg > Int.MAX_VALUE.toULong()) throw CborException("length argument too large")
    return arg.toInt()
}

/** Read the additional-information argument for a head byte whose low five bits are
 *  `low`, returning the argument value and the total head length (initial byte
 *  included). */
private fun readArg(b: ByteArray, off: Int, low: Int): Pair<ULong, Int> {
    return when {
        low < 24 -> Pair(low.toULong(), 1)
        low == 24 -> {
            requireAvailable(b, off, 2, "1-byte argument")
            Pair((b[off + 1].toInt() and 0xFF).toULong(), 2)
        }
        low == 25 -> {
            requireAvailable(b, off, 3, "2-byte argument")
            val v = ((b[off + 1].toInt() and 0xFF) shl 8) or (b[off + 2].toInt() and 0xFF)
            Pair(v.toULong(), 3)
        }
        low == 26 -> {
            requireAvailable(b, off, 5, "4-byte argument")
            var v = 0uL
            for (i in 1..4) v = (v shl 8) or (b[off + i].toInt() and 0xFF).toULong()
            Pair(v, 5)
        }
        low == 27 -> {
            requireAvailable(b, off, 9, "8-byte argument")
            var v = 0uL
            for (i in 1..8) v = (v shl 8) or (b[off + i].toInt() and 0xFF).toULong()
            Pair(v, 9)
        }
        // 28..31 (indefinite-length / reserved) are forbidden in CSIL envelopes.
        else -> throw CborException("indefinite or reserved additional info $low")
    }
}

private fun requireAvailable(b: ByteArray, off: Int, count: Int, what: String) {
    if (off < 0 || off > b.size || count > b.size - off) throw CborException("truncated $what")
}
