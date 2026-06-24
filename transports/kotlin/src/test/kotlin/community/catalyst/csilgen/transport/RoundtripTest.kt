// End-to-end and edge-case tests beyond the byte-exact conformance vectors: client/server
// over the loopback carrier, the framing guards, the canonical map ordering, and the
// sequence tracker semantics. These exercise the parts the vectors don't reach.
package community.catalyst.csilgen.transport

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

class RoundtripTest {
    @Test
    fun rpcClientServerRoundtrip() {
        // Wire the client's outbound queue to the server's inbound and vice versa by hand:
        // the client sends a request frame, the server consumes it and replies.
        val clientToServer = LoopbackFrameCarrier()
        val serverToClient = LoopbackFrameCarrier()

        val req = RpcRequest.of("Attestation", "deposit-claim", encodeValue(CMap(emptyList())))
        clientToServer.sendFrame(req.encode())

        val server = RpcServer(object : FrameCarrier {
            override fun sendFrame(bytes: ByteArray) = serverToClient.sendFrame(bytes)
            override fun recvFrame(): ByteArray? = clientToServer.takeOutbound()
        })
        val served = server.serveOne { r ->
            assertEquals("deposit-claim", r.op)
            Reply("DepositClaimResponse", encodeValue(CMap(emptyList())))
        }
        assertTrue(served)

        val resp = RpcResponse.decode(serverToClient.takeOutbound()!!)
        assertEquals(Status.OK, resp.status)
        assertEquals("DepositClaimResponse", resp.variant)
    }

    @Test
    fun rpcServerSurfacesTransportStatus() {
        val req = RpcRequest.of("S", "op", encodeValue(CMap(emptyList())))
        val carrier = object : FrameCarrier {
            var sent: ByteArray? = null
            override fun sendFrame(bytes: ByteArray) { sent = bytes }
            private var given = false
            override fun recvFrame(): ByteArray? = if (!given) { given = true; req.encode() } else null
        }
        val server = RpcServer(carrier)
        server.serveOne { TransportOutcome(Status.UNKNOWN_SERVICE_OR_OP, "no such op") }
        val resp = RpcResponse.decode(carrier.sent!!)
        assertEquals(Status.UNKNOWN_SERVICE_OR_OP, resp.status)
        val err = resp.asTransportError()
        assertTrue(err is StatusException)
        assertEquals("no such op", err.detail)
    }

    @Test
    fun frameTooLargeIsRejectedBeforeAllocating() {
        val out = ByteArrayOutputStream()
        assertFailsWith<FrameTooLargeException> {
            writeLengthPrefixed(out, ByteArray(17), max = 16)
        }
    }

    @Test
    fun lengthPrefixIsComparedUnsigned() {
        // A 4-byte prefix of 0x80000000 is a negative Int if narrowed naively; the guard
        // must compare it as unsigned and reject it against the 16 MiB max.
        val framed = byteArrayOf(0x80.toByte(), 0x00, 0x00, 0x00)
        assertFailsWith<FrameTooLargeException> {
            readLengthPrefixed(ByteArrayInputStream(framed), MAX_FRAME_DEFAULT)
        }
    }

    @Test
    fun cleanEofReturnsNull() {
        assertNull(readLengthPrefixed(ByteArrayInputStream(ByteArray(0)), MAX_FRAME_DEFAULT))
    }

    @Test
    fun streamCarrierRoundtripsAFrame() {
        val buffer = ByteArrayOutputStream()
        val tx = StreamCarrier(ByteArrayInputStream(ByteArray(0)), buffer)
        val payload = byteArrayOf(1, 2, 3, 4, 5)
        tx.sendFrame(payload)
        val rx = StreamCarrier(ByteArrayInputStream(buffer.toByteArray()), ByteArrayOutputStream())
        assertContentEquals(payload, rx.recvFrame())
    }

    @Test
    fun negativeIntDecodeGuard() {
        // Major type 1 with an 8-byte argument of 0xFFFFFFFFFFFFFFFF names a value below
        // Long.MIN_VALUE; decoding must reject it rather than silently wrap.
        val bytes = byteArrayOf(
            0x3B.toByte(), // major 1, additional info 27
            0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte(),
            0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte(),
        )
        assertFailsWith<CborException> { decodeEnvelope(bytes) }
    }

    @Test
    fun canonicalMapOrdersByEncodedKeyBytes() {
        // "v" (single-byte text) sorts before "service" (longer text) because shorter
        // encoded keys come first; insertion order here is deliberately reversed.
        val m = canonMap(
            listOf(
                CEntry(CText("service"), CText("World")),
                CEntry(CText("v"), CUInt(1uL)),
            ),
        )
        val first = (m.entries[0].key as CText).s
        assertEquals("v", first)
    }

    @Test
    fun trailingBytesAreRejected() {
        val good = RpcPush("World", "room-delta", encodeValue(CMap(emptyList()))).encode()
        assertFailsWith<CborException> { decodeEnvelope(good + byteArrayOf(0x00)) }
    }

    @Test
    fun statusPreservesUnknownHostCodes() {
        val s = Status.fromCode(64)
        assertEquals("other", s.statusName)
        assertEquals(64, s.code)
        assertEquals(s, Status.fromCode(64))
    }

    @Test
    fun helloNegotiatesPreferredSupportedProfile() {
        val hello = Hello(versions = listOf(VERSION), profiles = listOf("compact", "verbose"))
        val (v, p) = hello.negotiate(listOf(Profile.VERBOSE, Profile.COMPACT))!!
        assertEquals(VERSION, v)
        // The peer's preference order wins: "compact" is offered first and is supported.
        assertEquals(Profile.COMPACT, p)
    }

    @Test
    fun helloRejectsUnsupportedVersion() {
        val hello = Hello(versions = listOf(999uL), profiles = listOf("verbose"))
        assertNull(hello.negotiate(listOf(Profile.VERBOSE)))
    }

    @Test
    fun seqTrackerClassifies() {
        val t = SeqTracker()
        assertEquals(SeqEventKind.FIRST, t.observe(5uL, null).kind)
        val adv = t.observe(8uL, null)
        assertEquals(SeqEventKind.ADVANCED, adv.kind)
        assertEquals(2uL, adv.gap) // skipped 6 and 7
        assertEquals(SeqEventKind.LATE_OR_DUPLICATE, t.observe(8uL, null).kind)
        // seq 0 is unsequenced: a zero-gap advance that leaves the running sequence intact.
        assertEquals(SeqEventKind.ADVANCED, t.observe(0uL, null).kind)
    }

    @Test
    fun seqTrackerRestartOnlyWhenPriorEpochChanged() {
        val t = SeqTracker()
        // no-epoch -> first epoch is NOT a restart.
        assertEquals(SeqEventKind.FIRST, t.observe(1uL, 0u.toUByte()).kind)
        // epoch changes from a prior epoch -> restart.
        assertEquals(SeqEventKind.RESTART, t.observe(1uL, 1u.toUByte()).kind)
    }

    @Test
    fun compactDatagramEpochRoundtrip() {
        val d = CompactDatagram(opOrd = 2u.toUByte(), seq = 7u.toUShort(), body = byteArrayOf(9))
            .withEpoch(4u.toUByte())
        val decoded = CompactDatagram.decode(d.encode())
        assertEquals(d, decoded)
        assertEquals(4u.toUByte(), decoded.epoch)
    }

    @Test
    fun emptyResponsePayloadIsPresentTag24() {
        // A non-zero status carries an empty — but present — tag-24 payload, not an absent
        // field. Re-decoding yields an empty byte array, never a missing payload.
        val r = RpcResponse.transportError(Status.INTERNAL, "boom")
        val decoded = RpcResponse.decode(r.encode())
        assertContentEquals(ByteArray(0), decoded.payload)
        assertEquals("boom", decoded.error)
    }
}
