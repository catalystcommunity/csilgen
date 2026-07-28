// The configurable max-frame guard (conventions doc §5): a host sets the limit up or down
// through the carrier's public API, the limit applies to reads and writes alike, an oversized
// inbound length is rejected before allocation, and an invalid limit fails at construction
// rather than on the first frame.
package community.catalyst.csilgen.transport

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.InputStream
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

/** Counts bytes handed out, so a test can prove the guard fires before the body is read. */
private class CountingInputStream(bytes: ByteArray) : InputStream() {
    private val inner = ByteArrayInputStream(bytes)
    var readCount = 0
        private set

    override fun read(): Int {
        val b = inner.read()
        if (b != -1) readCount++
        return b
    }

    override fun read(b: ByteArray, off: Int, len: Int): Int {
        val n = inner.read(b, off, len)
        if (n > 0) readCount += n
        return n
    }
}

class MaxFrameTest {
    @Test
    fun defaultLimitAcceptsFrameBelowIt() {
        val out = ByteArrayOutputStream()
        val frame = ByteArray(1024) { 0xAB.toByte() }
        StreamCarrier(InputStream.nullInputStream(), out).sendFrame(frame)

        val reader = StreamCarrier(ByteArrayInputStream(out.toByteArray()), out)
        assertContentEquals(frame, reader.recvFrame())
    }

    @Test
    fun defaultLimitRejectsFrameAboveIt() {
        val out = ByteArrayOutputStream()
        val carrier = StreamCarrier(InputStream.nullInputStream(), out)
        assertFailsWith<FrameTooLargeException> {
            carrier.sendFrame(ByteArray(MAX_FRAME_DEFAULT + 1))
        }
        assertEquals(0, out.size(), "a rejected frame must not put bytes on the wire")
    }

    @Test
    fun largerCustomLimitAcceptsWhatDefaultRejects() {
        val out = ByteArrayOutputStream()
        val raised = MAX_FRAME_DEFAULT + 4096
        val frame = ByteArray(MAX_FRAME_DEFAULT + 1)
        StreamCarrier(InputStream.nullInputStream(), out, raised).sendFrame(frame)

        val reader = StreamCarrier(ByteArrayInputStream(out.toByteArray()), out, raised)
        assertEquals(frame.size, reader.recvFrame()?.size)
    }

    @Test
    fun smallerCustomLimitRejectsWhatDefaultAccepts() {
        val carrier = StreamCarrier(InputStream.nullInputStream(), ByteArrayOutputStream(), 64)
        assertFailsWith<FrameTooLargeException> { carrier.sendFrame(ByteArray(1024)) }
    }

    @Test
    fun oversizedIncomingLengthRejectedBeforeAllocation() {
        // A prefix claiming ~4 GiB followed by no body: if the guard ran after the read this
        // would allocate; it must fail on the prefix alone.
        val input = CountingInputStream(byteArrayOf(-1, -1, -1, -1))
        val carrier = StreamCarrier(input, ByteArrayOutputStream(), 4096)
        assertFailsWith<FrameTooLargeException> { carrier.recvFrame() }
        assertEquals(4, input.readCount, "guard must fire on the 4-byte prefix alone")
    }

    @Test
    fun invalidLimitsRejectedAtConstruction() {
        // MAX_FRAME_LIMIT is Int.MAX_VALUE, so no Int argument can exceed it; the reachable
        // invalid values are all at or below zero.
        for (limit in intArrayOf(0, -1, -4096, Int.MIN_VALUE)) {
            assertFailsWith<InvalidMaxFrameException>("limit $limit must be rejected") {
                StreamCarrier(InputStream.nullInputStream(), ByteArrayOutputStream(), limit)
            }
        }
    }

    @Test
    fun boundaryLimitsAccepted() {
        for (limit in intArrayOf(1, MAX_FRAME_DEFAULT, MAX_FRAME_LIMIT)) {
            val carrier =
                StreamCarrier(InputStream.nullInputStream(), ByteArrayOutputStream(), limit)
            assertEquals(limit, carrier.maxFrame)
        }
    }
}
