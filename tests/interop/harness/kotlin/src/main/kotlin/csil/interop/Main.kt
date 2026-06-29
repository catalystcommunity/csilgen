// Kotlin interop harness — mirrors the Rust reference (tests/interop/harness/rust).
//
// Runs as a server (binds a loopback port, serves one transport) or a client (connects,
// runs the case battery for one transport, prints one JSON result line). Stream transports
// (rpc/events) ride a loopback TCP socket wrapped in the lib StreamCarrier; datagrams ride
// loopback UDP via the JDK's DatagramSocket — both carrier-independent of the codec.
package csil.interop

import community.catalyst.csilgen.generated.*
import community.catalyst.csilgen.transport.Control
import community.catalyst.csilgen.transport.Datagram
import community.catalyst.csilgen.transport.Event
import community.catalyst.csilgen.transport.Hello
import community.catalyst.csilgen.transport.HelloAck
import community.catalyst.csilgen.transport.HandlerOutcome
import community.catalyst.csilgen.transport.Profile
import community.catalyst.csilgen.transport.Reply
import community.catalyst.csilgen.transport.RpcClient
import community.catalyst.csilgen.transport.RpcRequest
import community.catalyst.csilgen.transport.RpcServer
import community.catalyst.csilgen.transport.Status
import community.catalyst.csilgen.transport.StreamCarrier
import community.catalyst.csilgen.transport.TransportOutcome
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket

private const val SERVICE = "interop"
private const val TICKS = 3L

// ---------------------------------------------------------------------------
// Fixed language-neutral test vectors (see tests/interop/README.md).
// ---------------------------------------------------------------------------

private fun scalarsOk() = Scalars(
    i = -42L,
    u = 42uL,
    n = -7L,
    f = 3.5,
    t = "héllo 世界",
    raw = byteArrayOf(0x01, 0x02, 0xF0.toByte(), 0xFF.toByte()),
    flag = true,
    `when` = java.time.Instant.parse("2026-06-29T12:34:56Z"),
    amount = java.math.BigDecimal("123.45"),
)

private fun collectionsOk() = Collections(
    names = listOf("a", "b"),
    atLeastOne = listOf(1L, 2L, 3L),
    bounded = listOf(10L, 20L),
    exact3 = listOf(7L, 8L, 9L),
    scores = mapOf("x" to 1L, "y" to 2L),
    color = Color.Green,
    prio = Priority.V2,
    who = IdOrNameVariant0(4242uL),
    pair = listOf("p", 5L),
    triple = listOf("t", 9L, true),
    extra = mapOf("k" to CborValue.CText("v")),
)

private fun constrainedOk() = Constrained(
    code = "PRD-AB12CD",
    qty = 10uL,
    rate = java.math.BigDecimal("0.25"),
    password = "hunter2hunter2",
    tags = listOf("one", "two"),
)

private fun constrainedBad() = Constrained(
    code = "bad",
    qty = 0uL,
    rate = java.math.BigDecimal("9.9"),
    password = "x",
    tags = emptyList(),
)

private fun nestedOk() = Nested(
    inner = scalarsOk(),
    maybe = constrainedOk(),
    many = listOf(scalarsOk()),
)

// ---------------------------------------------------------------------------
// Result accumulation + JSON output.
// ---------------------------------------------------------------------------

private class Cases(private val transport: String) {
    private data class Case(val name: String, val ok: Boolean, val detail: String)
    private val out = ArrayList<Case>()

    fun pass(name: String) = out.add(Case(name, true, ""))
    fun fail(name: String, detail: String) = out.add(Case(name, false, detail))
    fun check(name: String, cond: Boolean, detail: String) =
        if (cond) pass(name) else fail(name, detail)

    fun emit() {
        val sb = StringBuilder()
        sb.append("{\"lang\":\"kotlin\",\"transport\":\"").append(transport).append("\",\"cases\":[")
        out.forEachIndexed { i, c ->
            if (i > 0) sb.append(',')
            sb.append("{\"name\":\"").append(c.name).append("\",\"ok\":").append(c.ok)
                .append(",\"detail\":").append(jsonStr(c.detail)).append('}')
        }
        sb.append("]}")
        println(sb)
        System.out.flush()
    }
}

private fun jsonStr(s: String): String {
    val sb = StringBuilder("\"")
    for (c in s) {
        when {
            c == '"' -> sb.append("\\\"")
            c == '\\' -> sb.append("\\\\")
            c == '\n' -> sb.append("\\n")
            c == '\t' -> sb.append("\\t")
            c == '\r' -> sb.append("\\r")
            c.code < 0x20 -> sb.append("\\u%04x".format(c.code))
            else -> sb.append(c)
        }
    }
    sb.append('"')
    return sb.toString()
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

/** Maps a status-0 reply whose variant is `ServiceError` onto the language error path
 *  (a thrown ClientError), per the transport conventions; otherwise hands the success
 *  payload to the typed client. */
private class RpcTransport(private val client: RpcClient) : Transport {
    override fun call(service: String, op: String, request: ByteArray): ByteArray {
        val resp = client.call(service, op, request)
        if (resp.variant == "ServiceError") {
            val se = serviceErrorFromCbor(resp.payload)
            throw ClientError(se.code, se.message)
        }
        return resp.payload
    }
}

private fun rpcDispatch(req: RpcRequest): HandlerOutcome = when (req.op) {
    "EchoScalars" -> reqReply(req.payload, { scalarsFromCbor(it) }) { Reply("Scalars", it.toCbor()) }
    "EchoCollections" -> reqReply(req.payload, { collectionsFromCbor(it) }) { Reply("Collections", it.toCbor()) }
    "EchoNested" -> reqReply(req.payload, { nestedFromCbor(it) }) {
        Reply("EchoNestedResult", EchoNestedResult(ok = true, echo = it).toCbor())
    }
    "ValidateConstrained" -> reqReply(req.payload, { constrainedFromCbor(it) }) { input ->
        try {
            input.validate()
            Reply("Constrained", input.toCbor())
        } catch (e: IllegalArgumentException) {
            // The typed error arm rides as a status-0 `ServiceError` variant.
            Reply("ServiceError", ServiceError(code = 422, message = e.message ?: "invalid").toCbor())
        }
    }
    else -> TransportOutcome(Status.UNKNOWN_SERVICE_OR_OP, "unknown op ${req.op}")
}

/** Decode the request (mapping a decode failure to a malformed-envelope status, like the
 *  reference) then build the reply via `handle`. */
private fun <T> reqReply(payload: ByteArray, decode: (ByteArray) -> T, handle: (T) -> HandlerOutcome): HandlerOutcome {
    val value = try {
        decode(payload)
    } catch (e: Exception) {
        return TransportOutcome(Status.MALFORMED_ENVELOPE, e.message ?: "malformed payload")
    }
    return handle(value)
}

private fun rpcServer(server: ServerSocket) {
    while (true) {
        val ch = try {
            server.accept()
        } catch (e: Exception) {
            break
        }
        val carrier = StreamCarrier(ch.getInputStream(), ch.getOutputStream())
        val rpc = RpcServer(carrier)
        try {
            while (rpc.serveOne(::rpcDispatch)) {
                // serve until the peer closes the connection
            }
        } catch (e: Exception) {
            // a dropped connection ends this peer; accept the next
        }
        try { ch.close() } catch (_: Exception) {}
    }
}

private fun rpcClient(ch: Socket): Cases {
    val cases = Cases("rpc")
    val carrier = StreamCarrier(ch.getInputStream(), ch.getOutputStream())
    val client = InteropClient(RpcTransport(RpcClient(carrier, false)))

    try {
        val r = client.echoScalars(scalarsOk())
        cases.check("echo-scalars/success", r == scalarsOk(), "mismatch: $r")
    } catch (e: Exception) {
        cases.fail("echo-scalars/success", e.message ?: e.toString())
    }
    try {
        val r = client.echoCollections(collectionsOk())
        cases.check("echo-collections/success", r == collectionsOk(), "mismatch: $r")
    } catch (e: Exception) {
        cases.fail("echo-collections/success", e.message ?: e.toString())
    }
    try {
        val r = client.echoNested(nestedOk())
        cases.check("echo-nested/success", r.ok && r.echo == nestedOk(), "mismatch: $r")
    } catch (e: Exception) {
        cases.fail("echo-nested/success", e.message ?: e.toString())
    }
    try {
        val r = client.validateConstrained(constrainedOk())
        cases.check("validate-constrained/success", r == constrainedOk(), "mismatch: $r")
    } catch (e: Exception) {
        cases.fail("validate-constrained/success", e.message ?: e.toString())
    }
    try {
        client.validateConstrained(constrainedBad())
        cases.fail("validate-constrained/failure", "server accepted invalid input")
    } catch (e: ClientError) {
        // The typed error arm must surface as a service error, not a transport error.
        cases.pass("validate-constrained/failure")
    } catch (e: Exception) {
        cases.fail("validate-constrained/failure", "expected service error, got $e")
    }
    return cases
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

private fun sendEvent(carrier: StreamCarrier, ev: Event) = carrier.sendFrame(ev.encode(Profile.VERBOSE))

private fun eventsServer(server: ServerSocket) {
    while (true) {
        val ch = try {
            server.accept()
        } catch (e: Exception) {
            break
        }
        val carrier = StreamCarrier(ch.getInputStream(), ch.getOutputStream())
        try {
            // Handshake: read $hello, reply $hello-ack (verbose).
            val helloFrame = carrier.recvFrame() ?: run { ch.close(); continue }
            val helloEv = Event.decode(helloFrame, Profile.VERBOSE)
            if (helloEv.event != Control.HELLO_NAME) { ch.close(); continue }
            Hello.decode(helloEv.payload)
            sendEvent(carrier, Event.verbose(null, Control.HELLO_ACK_NAME, HelloAck(1uL, "verbose").encode()))

            // Push N ticks (on-tick / server push).
            for (seq in 0 until TICKS) {
                sendEvent(carrier, Event.verbose(SERVICE, "OnTick", Tick(seq.toULong()).toCbor()))
            }

            // React loop.
            while (true) {
                val frame = carrier.recvFrame() ?: break
                val ev = Event.decode(frame, Profile.VERBOSE)
                when (ev.event) {
                    Control.PING_NAME -> sendEvent(carrier, Event.verbose(null, Control.PONG_NAME, ev.payload))
                    Control.CLOSE_NAME -> break
                    "Duplex" -> {
                        val s = scalarsFromCbor(ev.payload)
                        sendEvent(carrier, Event.verbose(SERVICE, "Duplex", s.toCbor()))
                    }
                    else -> sendEvent(carrier, Event.verbose(null, Control.ERROR_NAME, ByteArray(0)))
                }
            }
        } catch (e: Exception) {
            // peer dropped; accept the next connection
        }
        try { ch.close() } catch (_: Exception) {}
    }
}

private fun eventsClient(ch: Socket): Cases {
    val cases = Cases("events")
    val carrier = StreamCarrier(ch.getInputStream(), ch.getOutputStream())

    // Handshake.
    val hello = Hello(versions = listOf(1uL), profiles = listOf("verbose"), service = SERVICE)
    sendEvent(carrier, Event.verbose(null, Control.HELLO_NAME, hello.encode()))
    val ackOk = carrier.recvFrame()?.let { Event.decode(it, Profile.VERBOSE).event == Control.HELLO_ACK_NAME } ?: false
    cases.check("events/handshake", ackOk, "no \$hello-ack")

    // on-tick: expect TICKS pushes with seq 0..TICKS.
    var tickOk = true
    var detail = ""
    for (expect in 0 until TICKS) {
        val frame = carrier.recvFrame()
        if (frame == null) { tickOk = false; detail = "stream closed during ticks"; break }
        val ev = Event.decode(frame, Profile.VERBOSE)
        if (ev.event != "OnTick") { tickOk = false; detail = "expected OnTick got ${ev.event}"; break }
        val t = tickFromCbor(ev.payload)
        if (t.seq != expect.toULong()) { tickOk = false; detail = "tick seq ${t.seq} != $expect"; break }
    }
    cases.check("on-tick/success", tickOk, detail)

    // duplex: send Scalars, expect echo.
    sendEvent(carrier, Event.verbose(SERVICE, "Duplex", scalarsOk().toCbor()))
    val duplexFrame = carrier.recvFrame()
    if (duplexFrame == null) {
        cases.fail("duplex/success", "no echo")
    } else {
        val ev = Event.decode(duplexFrame, Profile.VERBOSE)
        if (ev.event != "Duplex") {
            cases.fail("duplex/success", "got ${ev.event}")
        } else {
            val s = scalarsFromCbor(ev.payload)
            cases.check("duplex/success", s == scalarsOk(), "echo mismatch")
        }
    }

    // unknown-method: send a bogus channel method, expect $error.
    sendEvent(carrier, Event.verbose(SERVICE, "Bogus", ByteArray(0)))
    val errFrame = carrier.recvFrame()
    val isErr = errFrame?.let { Event.decode(it, Profile.VERBOSE).event == Control.ERROR_NAME } ?: false
    cases.check("unknown-method/failure", isErr, "expected \$error")

    sendEvent(carrier, Event.verbose(null, Control.CLOSE_NAME, ByteArray(0)))
    return cases
}

// ---------------------------------------------------------------------------
// Datagrams (loopback UDP via the JDK's DatagramSocket)
// ---------------------------------------------------------------------------

private const val OP_ECHO_SCALARS = 0uL
private const val OP_ECHO_COLLECTIONS = 1uL
private const val OP_ERROR_SENTINEL = 255uL
private const val DGRAM_MAX = 65536

private fun datagramServer(sock: DatagramSocket) {
    val buf = ByteArray(DGRAM_MAX)
    while (true) {
        val packet = DatagramPacket(buf, buf.size)
        try {
            sock.receive(packet)
        } catch (e: Exception) {
            continue
        }
        val data = packet.data.copyOf(packet.length)
        val dg = try {
            Datagram.decode(data)
        } catch (e: Exception) {
            continue
        }
        val reply = when (dg.opOrd) {
            OP_ECHO_SCALARS -> try {
                Datagram(OP_ECHO_SCALARS, dg.seq, scalarsFromCbor(dg.payload).toCbor())
            } catch (e: Exception) {
                Datagram(OP_ERROR_SENTINEL, dg.seq, ByteArray(0))
            }
            OP_ECHO_COLLECTIONS -> try {
                Datagram(OP_ECHO_COLLECTIONS, dg.seq, collectionsFromCbor(dg.payload).toCbor())
            } catch (e: Exception) {
                Datagram(OP_ERROR_SENTINEL, dg.seq, ByteArray(0))
            }
            else -> Datagram(OP_ERROR_SENTINEL, dg.seq, ByteArray(0))
        }
        // Reply to the source of the packet we just received.
        val out = reply.encode()
        try {
            sock.send(DatagramPacket(out, out.size, packet.address, packet.port))
        } catch (e: Exception) {
            // best-effort reply; keep serving
        }
    }
}

private fun datagramClient(sock: DatagramSocket): Cases {
    val cases = Cases("datagrams")
    sock.soTimeout = 3000

    fun roundtrip(op: ULong, seq: ULong, payload: ByteArray): Datagram? {
        val out = Datagram(op, seq, payload).encode()
        sock.send(DatagramPacket(out, out.size))
        val buf = ByteArray(DGRAM_MAX)
        val packet = DatagramPacket(buf, buf.size)
        return try {
            sock.receive(packet)
            Datagram.decode(packet.data.copyOf(packet.length))
        } catch (e: Exception) {
            null
        }
    }

    val d1 = roundtrip(OP_ECHO_SCALARS, 1uL, scalarsOk().toCbor())
    when {
        d1 == null -> cases.fail("echo-scalars/success", "no reply")
        d1.opOrd != OP_ECHO_SCALARS -> cases.fail("echo-scalars/success", "op_ord ${d1.opOrd}")
        else -> cases.check("echo-scalars/success", scalarsFromCbor(d1.payload) == scalarsOk(), "payload mismatch")
    }

    val d2 = roundtrip(OP_ECHO_COLLECTIONS, 2uL, collectionsOk().toCbor())
    when {
        d2 == null -> cases.fail("echo-collections/success", "no reply")
        d2.opOrd != OP_ECHO_COLLECTIONS -> cases.fail("echo-collections/success", "op_ord ${d2.opOrd}")
        else -> cases.check("echo-collections/success", collectionsFromCbor(d2.payload) == collectionsOk(), "payload mismatch")
    }

    val d3 = roundtrip(99uL, 3uL, ByteArray(0))
    when (d3) {
        null -> cases.fail("bad-op-ord/failure", "no reply")
        else -> cases.check("bad-op-ord/failure", d3.opOrd == OP_ERROR_SENTINEL, "expected error sentinel, got op_ord ${d3.opOrd}")
    }
    return cases
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

private fun ready() {
    println("READY")
    System.out.flush()
}

private val LOOPBACK: InetAddress = InetAddress.getByName("127.0.0.1")

/** Bind a loopback TCP listener with SO_REUSEADDR, retrying briefly so a just-freed port
 *  from the previous server in the matrix is tolerated without hitting TIME_WAIT. */
private fun bindRetry(port: Int): ServerSocket {
    repeat(200) {
        try {
            val server = ServerSocket()
            server.reuseAddress = true
            server.bind(InetSocketAddress(LOOPBACK, port))
            return server
        } catch (e: Exception) {
            Thread.sleep(15)
        }
    }
    val server = ServerSocket()
    server.reuseAddress = true
    server.bind(InetSocketAddress(LOOPBACK, port))
    return server
}

private fun connectRetry(port: Int): Socket {
    repeat(200) {
        try {
            return Socket(LOOPBACK, port)
        } catch (e: Exception) {
            Thread.sleep(15)
        }
    }
    return Socket(LOOPBACK, port)
}

fun main(args: Array<String>) {
    if (args.size < 3) {
        System.err.println("usage: harness <server|client> <rpc|events|datagrams> <port>")
        kotlin.system.exitProcess(2)
    }
    val mode = args[0]
    val transport = args[1]
    val port = args[2].toInt()

    when {
        mode == "server" && transport == "datagrams" -> {
            val sock = DatagramSocket(InetSocketAddress(LOOPBACK, port))
            ready()
            datagramServer(sock)
        }
        mode == "server" -> {
            val server = bindRetry(port)
            ready()
            when (transport) {
                "rpc" -> rpcServer(server)
                "events" -> eventsServer(server)
                else -> { System.err.println("bad transport"); kotlin.system.exitProcess(2) }
            }
        }
        mode == "client" && transport == "datagrams" -> {
            // Bind an ephemeral local UDP port; the server replies to this source.
            val sock = DatagramSocket()
            sock.connect(InetSocketAddress(LOOPBACK, port))
            val cases = datagramClient(sock)
            cases.emit()
            sock.close()
        }
        mode == "client" -> {
            val ch = connectRetry(port)
            val cases = when (transport) {
                "rpc" -> rpcClient(ch)
                "events" -> eventsClient(ch)
                else -> { System.err.println("bad transport"); kotlin.system.exitProcess(2) }
            }
            cases.emit()
            ch.close()
        }
        else -> { System.err.println("bad mode/transport"); kotlin.system.exitProcess(2) }
    }
}
