// Carrier seams — the bring-your-own-carrier boundary (conventions doc §7).
//
// The library owns envelope codecs, framing, and lifecycle; the *carrier* (the
// byte/datagram transport) is injected. A host supplies QUIC, WebRTC, a platform media
// stack, or anything else by implementing one of these interfaces — without changing the
// library. Everything here is synchronous and blocking by design: NO coroutines. The
// host owns the I/O loop and any threads; a `recv...(): ByteArray?` returning null is the
// Kotlin-idiomatic clean end-of-stream signal (maps Go's `nil` / Python's `Optional`).
package community.catalyst.csilgen.transport

import java.io.EOFException
import java.io.InputStream
import java.io.OutputStream

/** Sends and receives one *delimited message* at a time. Used by CSIL-RPC and
 *  CSIL-Events. Built-ins frame with a 4-byte big-endian length prefix; a host may
 *  implement this over WebSocket binary frames, a WebTransport stream, etc. Two methods,
 *  so it is not a single-abstract-method `fun interface`. */
interface FrameCarrier {
    fun sendFrame(bytes: ByteArray)

    /** The next frame, or null at a clean end of stream. */
    fun recvFrame(): ByteArray?
}

/** Sends and receives one self-contained datagram (each within the channel MTU), with no
 *  delivery or ordering guarantee. Used by CSIL-Datagrams. */
interface DatagramCarrier {
    fun sendDatagram(bytes: ByteArray)

    /** The next datagram, or null if the carrier is closed. */
    fun recvDatagram(): ByteArray?
}

/** Write a 4-byte big-endian length prefix followed by `b` (CSIL stream framing),
 *  enforcing the max-frame guard before writing. */
fun writeLengthPrefixed(out: OutputStream, b: ByteArray, max: Int) {
    if (b.size > max) throw FrameTooLargeException(b.size.toLong(), max)
    val n = b.size
    val prefix = byteArrayOf(
        ((n ushr 24) and 0xFF).toByte(),
        ((n ushr 16) and 0xFF).toByte(),
        ((n ushr 8) and 0xFF).toByte(),
        (n and 0xFF).toByte(),
    )
    try {
        out.write(prefix)
        out.write(b)
        out.flush()
    } catch (e: java.io.IOException) {
        throw CarrierException(e.message ?: "write failed", e)
    }
}

/** Read one length-prefixed frame, enforcing the max-frame guard before allocating.
 *  Returns null at a clean EOF before any byte of a frame. */
fun readLengthPrefixed(input: InputStream, max: Int): ByteArray? {
    val lenBuf = ByteArray(4)
    val first = try {
        input.read()
    } catch (e: java.io.IOException) {
        throw CarrierException(e.message ?: "read failed", e)
    }
    // A clean EOF before any frame byte is an orderly end of stream.
    if (first == -1) return null
    lenBuf[0] = first.toByte()
    try {
        readFully(input, lenBuf, 1, 3)
    } catch (e: EOFException) {
        throw CarrierException("truncated length prefix", e)
    } catch (e: java.io.IOException) {
        throw CarrierException(e.message ?: "read failed", e)
    }
    // Compare the prefix as unsigned before narrowing: a length >= 0x80000000 would
    // become a negative Int that slips past a signed `> max` guard and then blow up in
    // ByteArray(n). Port of Go's exact guard.
    val length = ((lenBuf[0].toLong() and 0xFF) shl 24) or
        ((lenBuf[1].toLong() and 0xFF) shl 16) or
        ((lenBuf[2].toLong() and 0xFF) shl 8) or
        (lenBuf[3].toLong() and 0xFF)
    if (length > max.toLong()) throw FrameTooLargeException(length, max)
    val buf = ByteArray(length.toInt())
    try {
        readFully(input, buf, 0, buf.size)
    } catch (e: EOFException) {
        throw CarrierException("truncated frame body", e)
    } catch (e: java.io.IOException) {
        throw CarrierException(e.message ?: "read failed", e)
    }
    return buf
}

/** Read exactly `len` bytes into `buf` at `offset`, throwing EOFException on a short
 *  read — the blocking analog of Go's io.ReadFull. */
private fun readFully(input: InputStream, buf: ByteArray, offset: Int, len: Int) {
    var read = 0
    while (read < len) {
        val n = input.read(buf, offset + read, len - read)
        if (n == -1) throw EOFException()
        read += n
    }
}

/** A FrameCarrier over any byte stream (TCP, TLS, Unix socket), using the canonical
 *  4-byte length-prefix framing. */
class StreamCarrier(
    private val input: InputStream,
    private val output: OutputStream,
    private val maxFrame: Int = MAX_FRAME_DEFAULT,
) : FrameCarrier {
    override fun sendFrame(bytes: ByteArray) = writeLengthPrefixed(output, bytes, maxFrame)
    override fun recvFrame(): ByteArray? = readLengthPrefixed(input, maxFrame)
}

/** An in-memory FrameCarrier backed by queues of frames — for tests and for driving the
 *  codec without a socket. */
class LoopbackFrameCarrier : FrameCarrier {
    private val outbound = ArrayDeque<ByteArray>()
    private val inbound = ArrayDeque<ByteArray>()

    /** Queue a frame that a subsequent recvFrame will return. */
    fun pushInbound(b: ByteArray) {
        inbound.addLast(b.copyOf())
    }

    /** Take the next frame that was sent via sendFrame, or null if none. */
    fun takeOutbound(): ByteArray? = outbound.removeFirstOrNull()

    override fun sendFrame(bytes: ByteArray) {
        outbound.addLast(bytes.copyOf())
    }

    override fun recvFrame(): ByteArray? = inbound.removeFirstOrNull()
}

/** An in-memory DatagramCarrier — for tests and codec drives. */
class LoopbackDatagramCarrier : DatagramCarrier {
    private val outbound = ArrayDeque<ByteArray>()
    private val inbound = ArrayDeque<ByteArray>()

    fun pushInbound(b: ByteArray) {
        inbound.addLast(b.copyOf())
    }

    fun takeOutbound(): ByteArray? = outbound.removeFirstOrNull()

    override fun sendDatagram(bytes: ByteArray) {
        outbound.addLast(bytes.copyOf())
    }

    override fun recvDatagram(): ByteArray? = inbound.removeFirstOrNull()
}
